#![forbid(unsafe_code)]

//! Stdio composition of the repository search and reading-desk host APIs.
//! Repository tokens qualify query/hit IDs across explicit root replacement.
//! Only successful attachment replaces the old repository. Its readers live in
//! the independent desk, so detachment cannot invalidate bookmarks or history.

use super::{Failure, DeskSession, HostResponse, PathBuf, number, count, hex, raw_path};
use crate::host::{atlas_session::AtlasSessionOptions, atlas_search::AtlasSearchOptions,
    desk::repository::DeskRepository};
pub(super) use crate::host::desk::repository::DeskRepositoryError;

pub(super) struct RepositoryCommands { current: Option<(u64, DeskRepository)> }
impl RepositoryCommands {
    pub(super) fn new() -> Self { Self { current: None } }
    pub(super) fn token(&self) -> Option<u64> { self.current.as_ref().map(|(token, _)| *token) }
    pub(super) fn query(&self) -> Option<u64> {
        self.current.as_ref().and_then(|(_, repo)| repo.accepted_generation())
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
            ("repo-find" | "repo-find-text-hex", [token, generation, limit, bytes, text]) => {
                let decoded;
                let needle = if command == "repo-find-text-hex" {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let options = AtlasSearchOptions { max_matches: count(limit)?, max_source_bytes: count(bytes)?,
                    ..AtlasSearchOptions::default() };
                Ok(self.get(number(token)?)?.search(number(generation)?, needle, options, &mut *canceled)?)
            }
            ("repo-page", [token, generation, offset, limit]) => {
                Ok(self.get(number(token)?)?.page(number(generation)?, count(offset)?, count(limit)?, &mut *canceled)?)
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
