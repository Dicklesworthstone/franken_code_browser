#![forbid(unsafe_code)]

//! Stdio composition of the repository search and reading-desk host APIs.
//! Repository tokens qualify query/hit IDs across explicit root replacement.
//! Only successful attachment replaces the old repository. Its readers live in
//! the independent desk, so detachment cannot invalidate bookmarks or history.

use super::{Failure, DeskSession, HostResponse, PathBuf, number, count, hex, raw_path};
use crate::host::{atlas_session::AtlasSessionOptions, atlas_search::{AtlasSearchOptions, AtlasIndexOptions},
    desk::repository::DeskRepository};
use crate::output::{Output, OutputError};
use crate::{EXIT_OK, EXIT_PARTIAL};
pub(super) use crate::host::desk::repository::DeskRepositoryError;

pub(super) const WORK_HELP: &str = "\nIncremental repository work (no background runtime):\n\
  repo-work TOKEN | repo-progress TOKEN QUERY_GEN\n\
  repo-begin TOKEN QUERY_GEN LIMIT SOURCE_BYTES TEXT\n\
  repo-begin-text-hex TOKEN QUERY_GEN LIMIT SOURCE_BYTES UTF8_TEXT_HEX\n\
  repo-step TOKEN QUERY_GEN EXPECTED_STEP | repo-cancel TOKEN QUERY_GEN\n\
  repo-index-begin TOKEN INDEX_GEN FILE_LIMIT SOURCE_BYTES GRAM_LIMIT\n\
  repo-index-step TOKEN INDEX_GEN EXPECTED_STEP | repo-index-info TOKEN INDEX_GEN\n\
  repo-index-cancel TOKEN INDEX_GEN | repo-index-clear TOKEN NEW_GEN\n\
  repo-find-indexed TOKEN INDEX_GEN QUERY_GEN LIMIT VERIFY_BYTES TEXT\n\
  repo-begin-indexed TOKEN INDEX_GEN QUERY_GEN LIMIT VERIFY_BYTES TEXT\n\
Both indexed query commands also accept a -text-hex suffix.\n\
Begin admits work without reading source payloads. Step visits at most one file.\n\
Use step_count (queries) or build_steps (indexes), initially 0, as EXPECTED_STEP.\n\
A stale step is rejected; inspect progress instead of replaying blindly.\n\
Query, index-build and clear generations share one increasing repository counter.\n\
Index preparation captures up to 1 MiB per file; indexed queries reuse those\n\
bytes with zero live-source reads. An index is memory-only, not a live refresh.\n\
repo-page and repo-hit also accept running query generations. An early page's\n\
next_offset=null is not EOF for a still-running query. Old accepted rows survive\n\
canceled replacement work. Explicit cancel targets only the matching generation.\n\
Running replies retain exit_code=3 and search_complete=false; they do not make\n\
a later complete session exit partial. Terminal quota/coverage failures do.\n\
Quit/EOF with uncompleted work exits 3; finish, cancel, or detach explicitly.\n\
No query, index, or root permission is added to a desk checkpoint.\n";

pub(super) struct RepositoryCommands {
    current: Option<(u64, DeskRepository)>,
    progress_reply: bool,
}
impl RepositoryCommands {
    pub(super) fn new() -> Self { Self { current: None, progress_reply: false } }
    pub(super) fn start_request(&mut self) { self.progress_reply = false; }
    pub(super) fn is_progress_reply(&self) -> bool { self.progress_reply }
    pub(super) fn token(&self) -> Option<u64> { self.current.as_ref().map(|(token, _)| *token) }
    pub(super) fn query(&self) -> Option<u64> {
        self.current.as_ref().and_then(|(_, repo)| repo.accepted_generation())
    }
    fn has_pending(&self) -> bool { self.current.as_ref().is_some_and(|(_, r)| r.work_state().has_pending()) }
    pub(super) fn session_exit(&self, exit: u8) -> u8 {
        if exit == EXIT_OK && self.has_pending() { EXIT_PARTIAL } else { exit }
    }
    pub(super) fn encode_work(&self, out: &mut Output) -> Result<(), OutputError> {
        let work = self.current.as_ref().map(|(_, r)| r.work_state());
        for (key, value) in [("repository_pending_query_generation", work.and_then(|w| w.pending_query)),
            ("repository_index_generation", work.and_then(|w| w.accepted_index)),
            ("repository_pending_index_generation", work.and_then(|w| w.pending_index))] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?;
            match value { Some(n) => out.integer(n)?, None => out.literal("null")? }
        }
        out.literal(",\"repository_work_pending\":")?; out.boolean(self.has_pending())
    }
    fn get(&mut self, token: u64) -> Result<&mut DeskRepository, Failure> {
        let (current, repo) = self.current.as_mut().ok_or(Failure::Protocol("DESK_NO_REPOSITORY"))?;
        if *current != token { return Err(Failure::Protocol("DESK_STALE_REPOSITORY")); }
        Ok(repo)
    }
    pub(super) fn execute(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        command: &str, args: &[&str], canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        match (command, args) {
            ("repo-open" | "repo-open-hex", [path]) => {
                let path = if command == "repo-open-hex" { raw_path(path)? } else { PathBuf::from(*path) };
                // Stdio request IDs never repeat, including unsuccessful opens.
                // The existing desk has one owner; repository owners are disjoint
                // and checked against integer exhaustion, not recycled ordinals.
                let owner = desk.model().owner().get().checked_add(attempt)
                    .and_then(|n| fcb::ArenaOwnerId::new(n).ok()).ok_or(Failure::Protocol("DESK_REPOSITORY_IDENTITY_EXHAUSTED"))?;
                let mut candidate = DeskRepository::open(owner, &path, AtlasSessionOptions::default(), &mut *canceled)?;
                let response = candidate.info(&mut *canceled)?;
                self.current = Some((attempt, candidate));
                Ok(response)
            }
            ("repo-info", [token]) => Ok(self.get(number(token)?)?.info(&mut *canceled)?),
            ("repo-work", [token]) => Ok(desk.repository_work_response(self.get(number(token)?)?, &mut *canceled)?),
            ("repo-find" | "repo-find-text-hex" | "repo-begin" | "repo-begin-text-hex", [token, generation, limit, bytes, text]) => {
                let decoded;
                let needle = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let options = AtlasSearchOptions { max_matches: count(limit)?, max_source_bytes: count(bytes)?,
                    ..AtlasSearchOptions::default() };
                let generation = number(generation)?;
                let repo = self.get(number(token)?)?;
                let response = if command.starts_with("repo-begin") { repo.begin_query(generation, needle, options, &mut *canceled)? }
                    else { repo.search(generation, needle, options, &mut *canceled)? };
                self.progress_reply = repo.work_state().pending_query == Some(generation);
                Ok(response)
            }
            ("repo-page", [token, generation, offset, limit]) => {
                let generation = number(generation)?; let repo = self.get(number(token)?)?;
                let response = repo.page(generation, count(offset)?, count(limit)?, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_query == Some(generation); Ok(response)
            }
            ("repo-progress", [token, generation]) => {
                let generation = number(generation)?; let repo = self.get(number(token)?)?;
                let response = repo.page(generation, 0, 64, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_query == Some(generation); Ok(response)
            }
            ("repo-step", [token, generation, step]) => {
                let generation = number(generation)?; let repo = self.get(number(token)?)?;
                let response = repo.step_query(generation, number(step)?, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_query == Some(generation); Ok(response)
            }
            ("repo-cancel" | "repo-index-cancel", [token, generation]) => {
                let repo = self.get(number(token)?)?;
                if command == "repo-cancel" { repo.cancel_query(number(generation)?, &mut *canceled)?; }
                else { repo.cancel_index(number(generation)?, &mut *canceled)?; }
                Ok(desk.repository_work_response(repo, || false)?)
            }
            ("repo-index-begin", [token, generation, files, bytes, grams]) => {
                let generation = number(generation)?;
                let options = AtlasIndexOptions { max_files: count(files)?, max_source_bytes: count(bytes)?,
                    max_index_grams: count(grams)?, ..Default::default() };
                let repo = self.get(number(token)?)?;
                let response = repo.begin_index(generation, options, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_index == Some(generation); Ok(response)
            }
            ("repo-index-step", [token, generation, step]) => {
                let generation = number(generation)?; let repo = self.get(number(token)?)?;
                let response = repo.step_index(generation, number(step)?, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_index == Some(generation); Ok(response)
            }
            ("repo-index-info", [token, generation]) => {
                let generation = number(generation)?; let repo = self.get(number(token)?)?;
                let response = repo.index_info(generation, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_index == Some(generation); Ok(response)
            }
            ("repo-index-clear", [token, generation]) => Ok(self.get(number(token)?)?.clear_index(number(generation)?, &mut *canceled)?),
            ("repo-find-indexed" | "repo-find-indexed-text-hex" | "repo-begin-indexed" | "repo-begin-indexed-text-hex",
                [token, index, generation, limit, bytes, text]) => {
                let decoded;
                let needle = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let generation = number(generation)?; let repo = self.get(number(token)?)?;
                let response = if command.starts_with("repo-begin") {
                    repo.begin_indexed_query(generation, number(index)?, needle, count(limit)?, number(bytes)?, &mut *canceled)?
                } else { repo.search_indexed(generation, number(index)?, needle, count(limit)?, number(bytes)?, &mut *canceled)? };
                self.progress_reply = repo.work_state().pending_query == Some(generation); Ok(response)
            }
            ("repo-hit", [token, generation, hit]) => {
                let opened = self.get(number(token)?)?.open_hit(desk, expected, attempt,
                    number(generation)?, number(hit)?, &mut *canceled)?;
                // The typed receipt already represents an accepted mutation.
                // Encoding it has no further cancellation gate.
                Ok(desk.repository_open_response(&opened)?)
            }
            ("repo-clear", [token, generation]) => Ok(self.get(number(token)?)?.clear(number(generation)?, &mut *canceled)?),
            ("repo-close", [token]) => {
                self.get(number(token)?)?;
                let response = desk.state(&mut *canceled)?;
                self.current = None;
                Ok(response)
            }
            _ => Err(Failure::Protocol("DESK_REPOSITORY_COMMAND_SYNTAX")),
        }
    }
}
