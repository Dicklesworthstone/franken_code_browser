#![forbid(unsafe_code)]

//! Offline repository composition for the persistent stdio desk. A saved
//! repository coexists with live repository work, but has separate tokens,
//! queries and explicitly pinned disk indexes. There is no source-path grant.

use super::{Failure, DeskSession, HostResponse, Output, PathBuf, number, count, hex, raw_path};
use crate::output::OutputError;
use crate::host::saved_repository::{SavedRepositorySession, SavedRepositoryError, SnapshotLimits, Sha256Digest};
pub(super) use crate::host::saved_repository::desk::SavedDeskError;
use crate::host::saved_repository::desk::{SavedExpression, SavedExpressionOptions};
use fcb::search::expression::ExpressionError;

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
  saved-expression TOKEN EXPR_GEN LIMIT SCAN_BYTES EXPRESSION\n\
  saved-expression-hex TOKEN EXPR_GEN LIMIT SCAN_BYTES UTF8_EXPRESSION_HEX\n\
  saved-expression-page TOKEN EXPR_GEN OFFSET COUNT\n\
  saved-expression-hit TOKEN EXPR_GEN HIT_ID\n\
  saved-expression-clear TOKEN EXPR_GEN\n\
Expressions reuse the first-party 1024-byte/64-token query grammar: one primary\n\
word or quoted phrase, further required terms, -excluded terms, path: and lang:\n\
filters (including -path: and -lang:). Matching is case-sensitive. Predicates\n\
apply to the whole captured member, not merely a matching line. Regex is refused.\n\
LIMIT is 0..4096; zero probes existence. SCAN_BYTES bounds aggregate matching\n\
work across ALL terms/files, including rescans of retained bytes. Incomplete\n\
coverage/work-limit is not proof of absence. Expressions bypass attached FCBD\n\
indexes explicitly; literal queries still use their independently pinned index.\n\
Expression generations are independent of literal/index generations and increase\n\
within an attachment. Failed attempts consume their generation but retain the\n\
previous result. Clear targets the accepted expression generation. Only expression\n\
hit import changes desk revision; its selected evidence can be bookmarked/saved.\n\
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
pub(super) struct SavedCommands {
    current: Option<(u64, SavedRepositorySession)>,
    expression: Option<SavedExpression>,
    last_expression_attempt: u64,
}
impl SavedCommands {
    pub(super) fn new() -> Self { Self { current: None, expression: None, last_expression_attempt: 0 } }
    pub(super) fn encode_state(&self, out: &mut Output) -> Result<(), OutputError> {
        for (key, value) in [("saved_token", self.current.as_ref().map(|(token, _)| *token)),
            ("saved_owner", self.current.as_ref().map(|(_, s)| s.owner().get())),
            ("saved_query_generation", self.current.as_ref().and_then(|(_, s)| s.accepted_generation())),
            ("saved_index_generation", self.current.as_ref().and_then(|(_, s)| s.index_generation())),
            ("saved_expression_generation", self.expression.as_ref().map(SavedExpression::generation)),
            ("last_saved_expression_attempt", self.current.as_ref().map(|_| self.last_expression_attempt))] {
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
    fn expression_pair(&mut self, token: u64, generation: u64)
        -> Result<(&mut SavedRepositorySession, &SavedExpression), Failure> {
        let (active, saved) = self.current.as_mut().ok_or(Failure::Protocol("DESK_NO_SAVED_REPOSITORY"))?;
        if token != *active { return Err(Failure::Protocol("DESK_STALE_SAVED_REPOSITORY")); }
        let expression = self.expression.as_ref().ok_or(Failure::Protocol("DESK_NO_SAVED_EXPRESSION"))?;
        expression.validate(saved, generation)?;
        Ok((saved, expression))
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
                self.expression = None;
                self.last_expression_attempt = 0;
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
            ("saved-expression" | "saved-expression-hex", [token, generation, limit, bytes, text]) => {
                let token = number(token)?; self.get(token)?;
                let generation = number(generation)?;
                if generation == 0 || generation <= self.last_expression_attempt {
                    return Err(SavedDeskError::from(ExpressionError::StaleQuery).into());
                }
                self.last_expression_attempt = generation;
                let options = SavedExpressionOptions { max_hits: count(limit)?, max_scan_bytes: number(bytes)? };
                let decoded;
                let query = if command == "saved-expression-hex" {
                    decoded = String::from_utf8(hex(text, fcb::search::MAX_QUERY_LEN)?)
                        .map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let saved = self.get(token)?;
                let candidate = SavedExpression::prepare(saved, generation, query, options, &mut *canceled)?;
                let response = candidate.page(saved, generation, 0, 64, &mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.expression = Some(candidate);
                Ok(response)
            }
            ("saved-expression-page", [token, generation, first, limit]) => {
                let generation = number(generation)?;
                let (saved, expression) = self.expression_pair(number(token)?, generation)?;
                Ok(expression.page(saved, generation, count(first)?, count(limit)?, &mut *canceled)?)
            }
            ("saved-expression-hit", [token, generation, hit]) => {
                let generation = number(generation)?;
                let (saved, expression) = self.expression_pair(number(token)?, generation)?;
                let opened = expression.open_hit_desk(saved, desk, expected, attempt, generation, number(hit)?, &mut *canceled)?;
                Ok(saved.desk_open_response(desk, &opened)?)
            }
            ("saved-expression-clear", [token, generation]) => {
                self.expression_pair(number(token)?, number(generation)?)?;
                let response = desk.state(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.expression = None;
                Ok(response)
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
                self.expression = None;
                Ok(response)
            }
            _ => Err(Failure::Protocol("DESK_SAVED_COMMAND_SYNTAX")),
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
