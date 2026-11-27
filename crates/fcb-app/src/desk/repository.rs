#![forbid(unsafe_code)]

//! Stdio composition of the repository search and reading-desk host APIs.
//! Repository tokens qualify query/hit IDs across explicit root replacement.
//! Only successful attachment replaces the old repository. Its readers live in
//! the independent desk, so detachment cannot invalidate bookmarks or history.

use super::{Failure, DeskSession, HostResponse, PathBuf, number, count, hex, raw_path};
use crate::host::{atlas_session::{AtlasSessionOptions, MAX_ATLAS_DIRECTORY_BYTES, MAX_ATLAS_FILE_BYTES,
        MAX_ATLAS_SESSION_FILES, directory_path, file_path}, catalog_session::CatalogDiscoveryPolicy, atlas_search::{AtlasSearchOptions, AtlasIndexOptions},
    atlas_paths::{AtlasPathOptions, PathCase, PathMatchMode}, desk::repository::{DeskRepository, RepositoryDiscoveryScope}};
use crate::host::atlas_search::{AtlasSearchScope, AtlasSearchPathKind, AtlasStructureMode,
    AtlasStructureOptions, AtlasIndexedStructureOptions, MAX_SEARCH_SCOPE_PATH_BYTES};
use fcb::search::SymbolNameMode;
use crate::output::{Output, OutputError};
use crate::{EXIT_OK, EXIT_PARTIAL};
pub(super) use crate::host::desk::repository::DeskRepositoryError;

pub(super) const WORK_HELP: &str = "\nExplicit repository discovery:\n\
  repo-open-scoped ROOT POLICY FILE_LIMIT SCOPE SCOPE_VALUE\n\
  repo-open-scoped-hex ROOT_PATH_HEX POLICY FILE_LIMIT SCOPE SCOPE_VALUE\n\
POLICY is repository-rules or include-excluded; FILE_LIMIT is 1..20000.\n\
SCOPE is workspace with -, or directory/file with raw relative-path hex.\n\
Paths admit 1..2048 bytes; directory permits 1..31 components, file 1..32.\n\
Directory discovery starts there before spending its independent file allowance.\n\
File discovery admits that member directly, without enumerating its siblings.\n\
The original root grant, inherited rules and project-relative source paths remain.\n\
include-excluded reads no rule files; symlink and special-object refusal remains.\n\
Receipts distinguish discovery_scope from query scope. A complete folder/file\n\
catalog does not claim whole-project coverage. Failed/canceled candidates keep the old\n\
token and readers. Acceptance assigns the request ID as token; delivery failure\n\
after acceptance does not roll back attachment. Legacy repo-open\n\
keeps repository rules and the 4096-file cap. Replacing/detaching preserves readers.\n\
\nIncremental repository work (no background runtime):\n\
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
\nScoped literal and expression queries:\n\
  repo-begin-matching TOKEN QUERY_GEN CASE COUNT LIMIT SOURCE_BYTES SCOPE SCOPE_VALUE TEXT\n\
  repo-begin-expression TOKEN QUERY_GEN CASE COUNT LIMIT SOURCE_BYTES VERIFY_BYTES SCOPE SCOPE_VALUE EXPRESSION\n\
  repo-begin-indexed-matching TOKEN INDEX_GEN QUERY_GEN CASE COUNT LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE TEXT\n\
  repo-begin-indexed-expression TOKEN INDEX_GEN QUERY_GEN CASE COUNT LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE EXPRESSION\n\
Each also accepts a -text-hex suffix. CASE is sensitive or unicode-lowercase;\n\
lowercase matching preserves original byte ranges and is not full case folding.\n\
COUNT is bounded or count-all. LIMIT is 1..4096, or 0 for count-all with no rows.\n\
Count-all continues past retained-row limits, subject to coverage/work budgets.\n\
Inspect match_count_complete separately from retained_hits and truncated.\n\
SOURCE_BYTES is 1..33554432. VERIFY_BYTES is 0..33554432 for indexed literals,\n\
or 0..268435456 for expressions. Source capture remains capped at 32 MiB.\n\
Expressions support phrases, conjunctions, exclusions, path: and lang: filters;\n\
only expression commands parse operators. Regex is not qualified or enabled.\n\
\nLater captured-index result windows:\n\
  repo-begin-indexed-matching-window TOKEN INDEX_GEN QUERY_GEN CASE COUNT OFFSET LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE TEXT\n\
  repo-begin-indexed-expression-window TOKEN INDEX_GEN QUERY_GEN CASE COUNT OFFSET LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE EXPRESSION\n\
Both commands also accept a -text-hex suffix. OFFSET is a zero-based occurrence\n\
offset, distinct from repo-page's row offset within the retained window.\n\
Use next_match_offset with the same index, query and scope but a fresh QUERY_GEN.\n\
Skipped matches consume verification work, not retained-hit capacity. Counts\n\
include skipped occurrences; expressions qualify a file before counting them.\n\
result_offset, skipped_matches, has_more_matches and next_match_offset accompany\n\
the ordinary coverage/count fields. A null next offset after partial coverage\n\
does not prove there are no later matches. LIMIT=0 requires count-all and has\n\
no navigable continuation. Each window uses the exact indexed captures without\n\
reopening live source. Pending/failed/canceled windows preserve accepted rows;\n\
activate hits with their own query generation, never an earlier window's IDs.\n\
\nStructural queries:\n\
  repo-begin-structure TOKEN QUERY_GEN MODE NAME_MODE LIMIT SOURCE_BYTES SCOPE SCOPE_VALUE TEXT\n\
  repo-begin-indexed-structure TOKEN INDEX_GEN QUERY_GEN MODE NAME_MODE LIMIT VERIFY_BYTES SCOPE SCOPE_VALUE TEXT\n\
Both structural commands also accept a -text-hex suffix for a UTF-8 hex needle.\n\
MODE is symbols, headings or occurrences; NAME_MODE is exact, prefix or contains.\n\
Names are case-sensitive. Occurrences require exact mode and include comments\n\
and strings; symbols are heuristic candidates, never compiler-resolved facts.\n\
SCOPE is workspace (SCOPE_VALUE is -), directory/file (hex UTF-8 relative path),\n\
or visible (sorted unique lowercase raw-path hex CSV; - is explicitly empty).\n\
Visible membership is declared by the caller, not proof of native presentation.\n\
Scope is frozen before quotas; it cannot widen the repository's original grant.\n\
LIMIT is 1..4096. SOURCE_BYTES is 1..33554432; VERIFY_BYTES is 0..33554432.\n\
Extractor-specific per-file caps and unavailable sources remain explicit.\n\
Structural queries share text/index generations, repo-step/page/progress/cancel,\n\
and repo-hit activation. Indexed extraction reuses captures with no live reads,\n\
including captures without grams. Bytes and selections survive index clearing\n\
or repository detachment after activation in the Desk.\n\
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
No query, index, or root permission is added to a desk checkpoint.\n\
\nFind files without searching their contents:\n\
  repo-paths TOKEN PATH_GEN exact|prefix|fuzzy sensitive|unicode-lowercase LIMIT TEXT\n\
  repo-paths-hex TOKEN PATH_GEN MODE CASE LIMIT NATIVE_QUERY_HEX\n\
  repo-path-page TOKEN PATH_GEN OFFSET LIMIT\n\
  repo-path-select TOKEN PATH_GEN FILE_ID | repo-path-open TOKEN PATH_GEN FILE_ID\n\
  repo-path-clear TOKEN NEW_PATH_GEN\n\
Path queries have their own increasing generations, separate from text/index work.\n\
Use file_id from the path response, not a row number or displayed path label.\n\
Lookup, paging and selection read no source payloads and do not advance text work.\n\
repo-path-open explicitly captures current regular-file bytes (desk limit 4 MiB)\n\
and changes the desk revision. Unlike repo-hit, it does not claim old searched bytes.\n\
For documentation, find README.md, open its returned file_id, then doc-prepare.\n\
Raw-path hex queries preserve native names; unicode-lowercase is not full folding.\n";

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
            ("repository_pending_index_generation", work.and_then(|w| w.pending_index)),
            ("repository_path_generation", self.current.as_ref().and_then(|(_, r)| r.path_generation()))] {
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
            ("repo-open" | "repo-open-hex" | "repo-open-scoped" | "repo-open-scoped-hex", args) => {
                let (path, policy, options, discovery) = match (command, args) {
                    ("repo-open" | "repo-open-hex", [path]) =>
                        (*path, CatalogDiscoveryPolicy::RepositoryRules, AtlasSessionOptions::default(),
                            RepositoryDiscoveryScope::Workspace),
                    ("repo-open-scoped" | "repo-open-scoped-hex", [path, policy, files, scope, value]) => {
                        let policy = match *policy {
                            "repository-rules" => CatalogDiscoveryPolicy::RepositoryRules,
                            "include-excluded" => CatalogDiscoveryPolicy::IncludeExcluded,
                            _ => return Err(Failure::Protocol("DESK_REPOSITORY_POLICY")),
                        };
                        let max_files = count(files)?;
                        if !(1..=MAX_ATLAS_SESSION_FILES).contains(&max_files) {
                            return Err(Failure::Protocol("DESK_REPOSITORY_FILE_LIMIT"));
                        }
                        let discovery = match (*scope, *value) {
                            ("workspace", "-") => RepositoryDiscoveryScope::Workspace,
                            ("directory", value) => RepositoryDiscoveryScope::Directory(
                                directory_path(&hex(value, MAX_ATLAS_DIRECTORY_BYTES)?)
                                    .map_err(DeskRepositoryError::from)?),
                            ("file", value) => RepositoryDiscoveryScope::File(
                                file_path(&hex(value, MAX_ATLAS_FILE_BYTES)?)
                                    .map_err(DeskRepositoryError::from)?),
                            _ => return Err(Failure::Protocol("DESK_REPOSITORY_SCOPE")),
                        };
                        (*path, policy, AtlasSessionOptions { max_files, ..Default::default() }, discovery)
                    }
                    _ => return Err(Failure::Protocol("DESK_REPOSITORY_COMMAND_SYNTAX")),
                };
                let path = if command.ends_with("-hex") { raw_path(path)? } else { PathBuf::from(path) };
                // Stdio request IDs never repeat, including unsuccessful opens.
                // The existing desk has one owner; repository owners are disjoint
                // and checked against integer exhaustion, not recycled ordinals.
                let owner = desk.model().owner().get().checked_add(attempt)
                    .and_then(|n| fcb::ArenaOwnerId::new(n).ok()).ok_or(Failure::Protocol("DESK_REPOSITORY_IDENTITY_EXHAUSTED"))?;
                let mut candidate = match discovery {
                    RepositoryDiscoveryScope::Workspace =>
                        DeskRepository::open_with_policy(owner, &path, options, policy, &mut *canceled)?,
                    RepositoryDiscoveryScope::Directory(directory) =>
                        DeskRepository::open_directory_with_policy(owner, &path, directory,
                            options, policy, &mut *canceled)?,
                    RepositoryDiscoveryScope::File(file) =>
                        DeskRepository::open_file_with_policy(owner, &path, file,
                            options, policy, &mut *canceled)?,
                };
                let response = candidate.info(&mut *canceled)?;
                self.current = Some((attempt, candidate));
                Ok(response)
            }
            ("repo-info", [token]) => Ok(self.get(number(token)?)?.info(&mut *canceled)?),
            ("repo-work", [token]) => Ok(desk.repository_work_response(self.get(number(token)?)?, &mut *canceled)?),
            ("repo-paths" | "repo-paths-hex", [token, generation, mode, case, limit, text]) => {
                let options = AtlasPathOptions {
                    mode: match *mode { "exact" => PathMatchMode::Exact, "prefix" => PathMatchMode::Prefix,
                        "fuzzy" => PathMatchMode::Fuzzy, _ => return Err(Failure::Protocol("DESK_PATH_MODE")) },
                    case: match *case { "sensitive" => PathCase::Sensitive, "unicode-lowercase" => PathCase::UnicodeLowercase,
                        _ => return Err(Failure::Protocol("DESK_PATH_CASE")) },
                    max_results: count(limit)?,
                };
                let decoded;
                let needle = if command == "repo-paths-hex" { decoded = hex(text, 256)?; decoded.as_slice() }
                    else { text.as_bytes() };
                Ok(self.get(number(token)?)?.find_paths(number(generation)?, needle, options, &mut *canceled)?)
            }
            ("repo-path-page", [token, generation, offset, limit]) =>
                Ok(self.get(number(token)?)?.path_page(number(generation)?, count(offset)?, count(limit)?, &mut *canceled)?),
            ("repo-path-select" | "repo-path-open", [token, generation, file]) => {
                let repo = self.get(number(token)?)?;
                let file = fcb::FileId::new(repo.owner(), number(file)?).map_err(|_| Failure::Protocol("DESK_PATH_FILE_ID"))?;
                if command == "repo-path-select" { return Ok(repo.select_path(number(generation)?, file, &mut *canceled)?); }
                let opened = repo.open_path_hit(desk, expected, attempt, number(generation)?, file, &mut *canceled)?;
                Ok(desk.repository_path_open_response(&opened)?)
            }
            ("repo-path-clear", [token, generation]) =>
                Ok(self.get(number(token)?)?.clear_paths(number(generation)?, &mut *canceled)?),
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
            ("repo-begin-matching" | "repo-begin-matching-text-hex",
                [token, generation, case, counting, limit, bytes, scope, scope_value, text]) => {
                let scope = text_scope(scope, scope_value, case)?;
                let count_all = count_all(counting)?;
                let decoded;
                let needle = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let options = AtlasSearchOptions { max_matches: count(limit)?, max_source_bytes: count(bytes)?,
                    ..AtlasSearchOptions::default() };
                let generation = number(generation)?;
                let repo = self.get(number(token)?)?;
                let response = if count_all {
                    repo.begin_counted_query_scoped(generation, needle, scope, options, &mut *canceled)?
                } else { repo.begin_query_scoped(generation, needle, scope, options, &mut *canceled)? };
                self.progress_reply = repo.work_state().pending_query == Some(generation);
                Ok(response)
            }
            ("repo-begin-expression" | "repo-begin-expression-text-hex",
                [token, generation, case, counting, limit, bytes, verify, scope, scope_value, text]) => {
                let scope = text_scope(scope, scope_value, case)?;
                let count_all = count_all(counting)?;
                let decoded;
                let expression = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let options = AtlasSearchOptions { max_matches: count(limit)?, max_source_bytes: count(bytes)?,
                    ..AtlasSearchOptions::default() };
                let generation = number(generation)?;
                let repo = self.get(number(token)?)?;
                let response = if count_all {
                    repo.begin_expression_counted_scoped(generation, expression, scope, options,
                        number(verify)?, &mut *canceled)?
                } else { repo.begin_expression_scoped(generation, expression, scope, options,
                    number(verify)?, &mut *canceled)? };
                self.progress_reply = repo.work_state().pending_query == Some(generation);
                Ok(response)
            }
            ("repo-begin-indexed-matching" | "repo-begin-indexed-matching-text-hex"
                | "repo-begin-indexed-expression" | "repo-begin-indexed-expression-text-hex",
                [token, index, generation, case, counting, limit, bytes, scope, scope_value, text]) => {
                let scope = text_scope(scope, scope_value, case)?;
                let count_all = count_all(counting)?;
                let decoded;
                let needle = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let generation = number(generation)?;
                let index = number(index)?;
                let limit = count(limit)?;
                let bytes = number(bytes)?;
                let repo = self.get(number(token)?)?;
                let response = match (command.starts_with("repo-begin-indexed-expression"), count_all) {
                    (false, false) => repo.begin_indexed_query_scoped(generation, index, needle,
                        scope, limit, bytes, &mut *canceled)?,
                    (false, true) => repo.begin_indexed_query_counted_scoped(generation, index, needle,
                        scope, limit, bytes, &mut *canceled)?,
                    (true, false) => repo.begin_indexed_expression_scoped(generation, index, needle,
                        scope, limit, bytes, &mut *canceled)?,
                    (true, true) => repo.begin_indexed_expression_counted_scoped(generation, index, needle,
                        scope, limit, bytes, &mut *canceled)?,
                };
                self.progress_reply = repo.work_state().pending_query == Some(generation);
                Ok(response)
            }
            ("repo-begin-indexed-matching-window" | "repo-begin-indexed-matching-window-text-hex"
                | "repo-begin-indexed-expression-window" | "repo-begin-indexed-expression-window-text-hex",
                [token, index, generation, case, counting, offset, limit, bytes, scope, scope_value, text]) => {
                let scope = text_scope(scope, scope_value, case)?;
                let count_all = count_all(counting)?;
                let decoded;
                let needle = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let generation = number(generation)?;
                let index = number(index)?;
                let offset = number(offset)?;
                let limit = count(limit)?;
                let bytes = number(bytes)?;
                let repo = self.get(number(token)?)?;
                let response = repo.begin_indexed_window_scoped(generation, index, needle, scope,
                    command.starts_with("repo-begin-indexed-expression"), count_all, offset,
                    limit, bytes, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_query == Some(generation);
                Ok(response)
            }
            ("repo-begin-structure" | "repo-begin-structure-text-hex",
                [token, generation, mode, name_mode, limit, bytes, scope, scope_value, text]) => {
                let mode = structure_mode(mode)?;
                let name_mode = structure_name_mode(name_mode)?;
                let scope = structure_scope(scope, scope_value)?;
                let decoded;
                let needle = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let options = AtlasStructureOptions { mode, name_mode,
                    limits: AtlasSearchOptions { max_matches: count(limit)?, max_source_bytes: count(bytes)?,
                        ..AtlasSearchOptions::default() } };
                let generation = number(generation)?;
                let repo = self.get(number(token)?)?;
                let response = repo.begin_structure(generation, needle, scope, options, &mut *canceled)?;
                self.progress_reply = repo.work_state().pending_query == Some(generation);
                Ok(response)
            }
            ("repo-begin-indexed-structure" | "repo-begin-indexed-structure-text-hex",
                [token, index, generation, mode, name_mode, limit, bytes, scope, scope_value, text]) => {
                let mode = structure_mode(mode)?;
                let name_mode = structure_name_mode(name_mode)?;
                let scope = structure_scope(scope, scope_value)?;
                let decoded;
                let needle = if command.ends_with("text-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let options = AtlasIndexedStructureOptions { mode, name_mode,
                    max_matches: count(limit)?, max_scan_bytes: number(bytes)? };
                let generation = number(generation)?;
                let repo = self.get(number(token)?)?;
                let response = repo.begin_indexed_structure(generation, number(index)?, needle,
                    scope, options, &mut *canceled)?;
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

fn structure_mode(mode: &str) -> Result<AtlasStructureMode, Failure> {
    match mode {
        "symbols" => Ok(AtlasStructureMode::Symbols),
        "headings" => Ok(AtlasStructureMode::Headings),
        "occurrences" => Ok(AtlasStructureMode::Occurrences),
        _ => Err(Failure::Protocol("DESK_STRUCTURE_MODE")),
    }
}
fn structure_name_mode(mode: &str) -> Result<SymbolNameMode, Failure> {
    match mode {
        "exact" => Ok(SymbolNameMode::Exact),
        "prefix" => Ok(SymbolNameMode::Prefix),
        "contains" => Ok(SymbolNameMode::Contains),
        _ => Err(Failure::Protocol("DESK_STRUCTURE_NAME_MODE")),
    }
}

/// Scope only narrows the repository's already authorized catalog. Directory
/// and file values are hex-encoded UTF-8 paths for lossless TAB framing; the
/// engine validates component boundaries without filesystem access. Visible
/// membership uses its canonical raw-path hex CSV and remains host-declared,
/// not evidence that a native frame was displayed. "-" explicitly means empty
/// workspace/visible payload; it never broadens an invalid file/directory scope.
fn structure_scope(kind: &str, value: &str) -> Result<AtlasSearchScope, Failure> {
    query_scope(kind, value, "DESK_STRUCTURE_SCOPE")
}
fn text_scope(kind: &str, value: &str, case: &str) -> Result<AtlasSearchScope, Failure> {
    let case_sensitive = case_sensitive(case)?;
    Ok(query_scope(kind, value, "DESK_SEARCH_SCOPE")?.with_case_sensitive(case_sensitive))
}
pub(super) fn case_sensitive(case: &str) -> Result<bool, Failure> {
    match case {
        "sensitive" => Ok(true),
        "unicode-lowercase" => Ok(false),
        _ => Err(Failure::Protocol("DESK_SEARCH_CASE")),
    }
}
pub(super) fn count_all(mode: &str) -> Result<bool, Failure> {
    match mode {
        "bounded" => Ok(false),
        "count-all" => Ok(true),
        _ => Err(Failure::Protocol("DESK_SEARCH_COUNT_MODE")),
    }
}
fn query_scope(kind: &str, value: &str, invalid: &'static str) -> Result<AtlasSearchScope, Failure> {
    let scope = AtlasSearchScope::all();
    match kind {
        "workspace" if value == "-" => Ok(scope),
        "visible" => scope.with_path(AtlasSearchPathKind::Visible, if value == "-" { "" } else { value })
            .map_err(DeskRepositoryError::from).map_err(Failure::from),
        "directory" | "file" => {
            let path = hex(value, MAX_SEARCH_SCOPE_PATH_BYTES)?;
            let kind = if kind == "directory" { AtlasSearchPathKind::Directory } else { AtlasSearchPathKind::File };
            scope.with_raw_path(kind, &path).map_err(DeskRepositoryError::from).map_err(Failure::from)
        }
        _ => Err(Failure::Protocol(invalid)),
    }
}
