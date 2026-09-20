#![forbid(unsafe_code)]

//! Offline repository composition for the persistent stdio desk. A saved
//! repository coexists with live repository work, but has separate tokens,
//! queries and explicitly pinned disk indexes. There is no source-path grant.

use super::{Failure, DeskSession, HostResponse, Output, PathBuf, number, count, hex, raw_path};
use crate::output::OutputError;
use crate::host::saved_repository::{SavedRepositorySession, SavedRepositoryError, SnapshotLimits, Sha256Digest};
pub(super) use crate::host::saved_repository::desk::SavedDeskError;

pub(super) const SAVED_HELP: &str = "\nOffline FCBS repositories (not FCBK reading-desk checkpoints):\n\
  saved-open ARCHIVE | saved-open-hex ARCHIVE_PATH_HEX\n\
  saved-info TOKEN | saved-members TOKEN FIRST_MEMBER COUNT\n\
  saved-find TOKEN QUERY_GEN LIMIT TEXT\n\
  saved-find-text-hex TOKEN QUERY_GEN LIMIT UTF8_TEXT_HEX\n\
  saved-page TOKEN QUERY_GEN OFFSET COUNT\n\
  saved-hit TOKEN QUERY_GEN HIT_ID | saved-member TOKEN MEMBER\n\
  saved-clear TOKEN NEW_QUERY_GEN | saved-close TOKEN\n\
  saved-index-attach TOKEN INDEX_GEN INDEX_FILE TRUSTED_PIN_HEX\n\
  saved-index-attach-hex TOKEN INDEX_GEN INDEX_PATH_HEX TRUSTED_PIN_HEX\n\
  saved-index-detach TOKEN NEW_INDEX_GEN\n\
Open validates the archive once; searches verify one member at a time. Defaults\n\
are 80 MiB archive, 64 MiB total captured source, 1 MiB per member, 65536 members.\n\
Members and page offsets are zero-based; hit IDs are one-based. Use the returned\n\
saved_token and query generation, never IDs from an earlier archive attachment.\n\
Member pages contain 1..128 rows. Literal queries return at most 1..4096 hits.\n\
A selected member is verified again before import; no live source path is opened.\n\
Missing/unsupported members and truncation remain explicit incomplete coverage.\n\
Search is synchronous worker work, not a background task or fixed-latency step.\n\
Hit/member import changes the desk revision. Other saved commands do not.\n\
Imported readers, selections, previews, bookmarks and checkpoint saves survive\n\
detachment. Repeated hits/queries reuse the same retained member identity.\n\
The FCBD pin must come from its trusted build receipt, not the opened index file.\n\
Attach validates archive binding; verified index pages use the existing cache.\n\
Query and index generations are separate and strictly increasing within this\n\
saved attachment. Live repository queries are independent and are not advanced.\n\
No saved archive handle, index or live-root permission enters a desk checkpoint.\n";

impl From<SavedRepositoryError> for Failure {
    fn from(error: SavedRepositoryError) -> Self { Self::Saved(SavedDeskError::Saved(error)) }
}
pub(super) struct SavedCommands { current: Option<(u64, SavedRepositorySession)> }
impl SavedCommands {
    pub(super) fn new() -> Self { Self { current: None } }
    pub(super) fn encode_state(&self, out: &mut Output) -> Result<(), OutputError> {
        for (key, value) in [("saved_token", self.current.as_ref().map(|(token, _)| *token)),
            ("saved_owner", self.current.as_ref().map(|(_, s)| s.owner().get())),
            ("saved_query_generation", self.current.as_ref().and_then(|(_, s)| s.accepted_generation())),
            ("saved_index_generation", self.current.as_ref().and_then(|(_, s)| s.index_generation()))] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?;
            match value { Some(n) => out.integer(n)?, None => out.literal("null")? }
        }
        Ok(())
    }
    fn get(&mut self, token: u64) -> Result<&mut SavedRepositorySession, Failure> {
        let (active, saved) = self.current.as_mut().ok_or(Failure::Protocol("DESK_NO_SAVED_REPOSITORY"))?;
        if token != *active { return Err(Failure::Protocol("DESK_STALE_SAVED_REPOSITORY")); }
        Ok(saved)
    }
    pub(super) fn execute(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        command: &str, args: &[&str], canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        match (command, args) {
            ("saved-open" | "saved-open-hex", [path]) => {
                let path = if command == "saved-open-hex" { raw_path(path)? } else { PathBuf::from(*path) };
                // Requests are unique across saved AND live root attachment.
                let owner = desk.model().owner().get().checked_add(attempt)
                    .and_then(|n| fcb::ArenaOwnerId::new(n).ok()).ok_or(Failure::Protocol("DESK_SAVED_IDENTITY_EXHAUSTED"))?;
                let mut candidate = SavedRepositorySession::open(owner, &path, SnapshotLimits::default(), &mut *canceled)?;
                let response = candidate.info(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.current = Some((attempt, candidate));
                Ok(response)
            }
            ("saved-info", [token]) => Ok(self.get(number(token)?)?.info(&mut *canceled)?),
            ("saved-members", [token, first, limit]) => Ok(self.get(number(token)?)?.members(count(first)?, count(limit)?, &mut *canceled)?),
            ("saved-find" | "saved-find-text-hex", [token, generation, limit, text]) => {
                let decoded;
                let needle = if command == "saved-find-text-hex" {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                Ok(self.get(number(token)?)?.search(number(generation)?, needle, count(limit)?, &mut *canceled)?)
            }
            ("saved-page", [token, generation, first, limit]) =>
                Ok(self.get(number(token)?)?.results(number(generation)?, count(first)?, count(limit)?, &mut *canceled)?),
            ("saved-hit", [token, generation, hit]) => {
                let saved = self.get(number(token)?)?;
                let opened = saved.open_hit_desk(desk, expected, attempt, number(generation)?, number(hit)?, &mut *canceled)?;
                Ok(saved.desk_open_response(desk, &opened)?)
            }
            ("saved-member", [token, ordinal]) => {
                let saved = self.get(number(token)?)?;
                let opened = saved.open_member_desk(desk, expected, attempt, count(ordinal)?, &mut *canceled)?;
                Ok(saved.desk_open_response(desk, &opened)?)
            }
            ("saved-clear", [token, generation]) => Ok(self.get(number(token)?)?.clear_results(number(generation)?, &mut *canceled)?),
            ("saved-index-attach" | "saved-index-attach-hex", [token, generation, path, pin]) => {
                if pin.len() != 64 { return Err(Failure::Protocol("DESK_SAVED_INDEX_PIN")); }
                let pin: [u8; 32] = hex(pin, 32)?.try_into().map_err(|_| Failure::Protocol("DESK_SAVED_INDEX_PIN"))?;
                let path = if command == "saved-index-attach-hex" { raw_path(path)? } else { PathBuf::from(*path) };
                Ok(self.get(number(token)?)?.attach_index(number(generation)?, &path, Sha256Digest::new(pin), &mut *canceled)?)
            }
            ("saved-index-detach", [token, generation]) => Ok(self.get(number(token)?)?.detach_index(number(generation)?, &mut *canceled)?),
            ("saved-close", [token]) => {
                self.get(number(token)?)?;
                let response = desk.state(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.current = None;
                Ok(response)
            }
            _ => Err(Failure::Protocol("DESK_SAVED_COMMAND_SYNTAX")),
        }
    }
}
