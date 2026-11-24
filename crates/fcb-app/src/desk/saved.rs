#![forbid(unsafe_code)]

//! Offline repository composition for the persistent stdio desk. A saved
//! repository coexists with live repository work, but has separate tokens,
//! queries and explicitly pinned disk indexes. There is no source-path grant.

use super::{Failure, DeskSession, HostResponse, Output, PathBuf, number, count, hex, raw_path};
use crate::output::OutputError;
use crate::host::saved_repository::{SavedRepositorySession, SavedRepositoryError, SnapshotLimits, Sha256Digest};
pub(super) use crate::host::saved_repository::desk::SavedDeskError;
use crate::host::saved_repository::desk::{SavedExpression, SavedExpressionOptions};
use crate::host::saved_repository::imports::{SavedImportError, SavedImportDirection, SavedImportSide};
use crate::host::saved_repository::relationships::{SavedRelationshipError, RelationshipDirection, RelationshipSide};
use crate::host::saved_repository::paths::{SavedPathOptions, PathCase, PathMatchMode, MAX_SAVED_PATH_QUERY_BYTES};
use crate::host::saved_repository::structure::{StructureMode, SymbolNameMode};
use fcb::search::expression::ExpressionError;

pub(super) const SAVED_HELP: &str = "\nOffline FCBS repositories (not FCBK reading-desk checkpoints):\n\
  saved-open ARCHIVE | saved-open-hex ARCHIVE_PATH_HEX\n\
  saved-info TOKEN | saved-members TOKEN FIRST_MEMBER COUNT\n\
  saved-find TOKEN QUERY_GEN LIMIT TEXT\n\
  saved-find-text-hex TOKEN QUERY_GEN LIMIT UTF8_TEXT_HEX\n\
  saved-find-window TOKEN QUERY_GEN LIMIT CASE COUNT SKIP_MATCHES TEXT\n\
  saved-page TOKEN QUERY_GEN OFFSET COUNT\n\
  saved-hit TOKEN QUERY_GEN HIT_ID | saved-member TOKEN MEMBER\n\
  saved-clear TOKEN NEW_QUERY_GEN | saved-close TOKEN\n\
  saved-paths TOKEN PATH_GEN exact|prefix|fuzzy sensitive|unicode-lowercase LIMIT WORK_UNITS TEXT\n\
  saved-paths-hex TOKEN PATH_GEN MODE CASE LIMIT WORK_UNITS NATIVE_QUERY_HEX\n\
  saved-path-page TOKEN PATH_GEN OFFSET COUNT\n\
  saved-path-hit TOKEN PATH_GEN HIT_ID | saved-path-clear TOKEN NEW_PATH_GEN\n\
  saved-structure TOKEN STRUCT_GEN symbols|headings|occurrences exact|prefix|contains LIMIT SOURCE_BYTES SKIP_MATCHES TEXT\n\
  saved-structure-text-hex TOKEN STRUCT_GEN MODE NAME_MODE LIMIT SOURCE_BYTES SKIP_MATCHES UTF8_TEXT_HEX\n\
  saved-structure-window TOKEN STRUCT_GEN MODE NAME_MODE LIMIT SOURCE_BYTES CASE COUNT SKIP_MATCHES TEXT\n\
  saved-structure-page TOKEN STRUCT_GEN OFFSET COUNT\n\
  saved-structure-hit TOKEN STRUCT_GEN HIT_ID | saved-structure-clear TOKEN NEW_STRUCT_GEN\n\
  saved-index-attach TOKEN INDEX_GEN INDEX_FILE TRUSTED_PIN_HEX\n\
  saved-index-attach-hex TOKEN INDEX_GEN INDEX_PATH_HEX TRUSTED_PIN_HEX\n\
  saved-index-detach TOKEN NEW_INDEX_GEN\n\
  saved-expression TOKEN EXPR_GEN LIMIT SCAN_BYTES EXPRESSION\n\
  saved-expression-hex TOKEN EXPR_GEN LIMIT SCAN_BYTES UTF8_EXPRESSION_HEX\n\
  saved-expression-window TOKEN EXPR_GEN LIMIT SCAN_BYTES CASE COUNT SKIP_MATCHES EXPRESSION\n\
  saved-expression-page TOKEN EXPR_GEN OFFSET COUNT\n\
  saved-expression-hit TOKEN EXPR_GEN HIT_ID\n\
  saved-expression-clear TOKEN EXPR_GEN\n\
  saved-links-build TOKEN REL_GEN MAX_LINKS MAX_SOURCE_BYTES\n\
  saved-links-page TOKEN REL_GEN OFFSET COUNT\n\
  saved-links-from TOKEN REL_GEN MEMBER OFFSET COUNT\n\
  saved-backlinks TOKEN REL_GEN MEMBER OFFSET COUNT\n\
  saved-link-source TOKEN REL_GEN LINK_ID | saved-link-target TOKEN REL_GEN LINK_ID\n\
  saved-links-clear TOKEN NEW_REL_GEN\n\
  saved-imports-build TOKEN IMPORT_GEN MAX_LINKS MAX_SOURCE_BYTES\n\
  saved-imports-page TOKEN IMPORT_GEN OFFSET COUNT\n\
  saved-imports-from TOKEN IMPORT_GEN MEMBER OFFSET COUNT\n\
  saved-importers TOKEN IMPORT_GEN MEMBER OFFSET COUNT\n\
  saved-import-source TOKEN IMPORT_GEN LINK_ID | saved-import-target TOKEN IMPORT_GEN LINK_ID\n\
  saved-imports-clear TOKEN NEW_IMPORT_GEN\n\
Saved Quick Open ranks archive-directory native names without reading source.\n\
LIMIT is 0..256; WORK_UNITS is 0..4294967296 (conservative ranking work, not time).\n\
Raw hex preserves non-UTF8 names. Unavailable members can match names but cannot\n\
open. A complete top-k can omit matches without incomplete coverage; a work\n\
limit makes the result partial. Path activation selects no source-text range.\n\
Structural search uses verified captured sources: symbols are heuristic outline\n\
candidates, headings use the qualified Markdown extractor, and occurrences are\n\
whole-token text candidates including comments/literals, never compiler resolution.\n\
Window commands accept CASE sensitive|unicode-lowercase, COUNT bounded|count-all,\n\
and a zero-based SKIP_MATCHES. Append -hex to any window command for UTF8 hex.\n\
Use count-all with LIMIT=0 for counts alone. Check matches_seen_exact before\n\
treating a total as exact. To read the next window use next_match_offset with a\n\
new query generation. In a tab-delimited session after opening archive token 1,\n\
for example, 0\tsaved-find-window\t1\t1\t5\tunicode-lowercase\tcount-all\t0\tReader\n\
returns up to five hits. Follow the same query with generation 2 and its returned\n\
offset. Use saved-hit with the new generation and hit_id to open captured bytes.\n\
Original commands default to sensitive bounded matching. Structural occurrences\n\
require sensitive exact mode. The structural result limit is 0..4096;\n\
SOURCE_BYTES is 0..67108864 with additional per-language extraction limits.\n\
SKIP_MATCHES requests a later window with a NEW generation; follow the returned\n\
next_match_offset. Retained page OFFSET is different and performs no extraction.\n\
Zero limits probe existence. Incomplete coverage is never proof of absence.\n\
PATH_GEN and STRUCT_GEN are independent of each other and all other saved query\n\
generations. Search/clear attempts increase; failed work preserves accepted rows.\n\
Use the returned one-based hit_id, not member ordinals or displayed labels.\n\
Only hit activation changes the Desk revision. It verifies the complete member\n\
again, then retains exact bytes and selections for notes/bookmarks/checkpoints.\n\
Source imports reuse the shared Rust/JS/TS/JSX/TSX/Python lexical extractor\n\
and closed saved-manifest resolver. Evidence selects the complete exact original\n\
statement in UTF8/UTF16 source. Captured targets open at byte zero. External,\n\
missing, ambiguous, unresolved and unavailable targets cannot open. This does\n\
not establish compiler resolution or execute code. Build reads at most\n\
MAX_SOURCE_BYTES (0..32 MiB), with 128 KiB per eligible source and 1..4096\n\
retained statements; omission/diagnostic counters keep coverage explicit.\n\
IMPORT_GEN is independent of Markdown/text/index/expression generations. Build\n\
and clear attempts increase; failures preserve the last accepted projection.\n\
Import pages read metadata only; COUNT is 1..128. Member ordinals start at zero,\n\
link IDs at one. Only source/target activation changes the Desk revision.\n\
Activated bytes and exact selections can be annotated, revisited and saved\n\
after archive detachment; no import projection or archive handle is persisted.\n\
Documentation links use the pinned Markdown parser and exact archive-member\n\
resolution. Link evidence is the whole original captured Markdown document,\n\
not a parser block position or inline token. Build scans at most MAX_SOURCE_BYTES of Markdown (up to 32 MiB,\n\
256 KiB per document), retaining at most 1..4096 links. Read completeness and\n\
omission counters: partial coverage is not proof that a file has no backlinks.\n\
REL_GEN is independent of text/index/expression generations; build/clear\n\
attempts strictly increase. Failed replacement keeps the accepted projection.\n\
Link pages are metadata-only; COUNT is 1..128. Member ordinals are zero-based,\n\
link IDs one-based. Only exact captured targets can open. Target navigation\n\
starts at byte zero; any URL fragment remains explicitly unresolved. External\n\
links never fetch URLs or open live paths. Link source/target imports change\n\
the desk revision and retain exact saved bytes; other link commands do not.\n\
Expressions reuse the first-party 1024-byte/64-token query grammar: one primary\n\
word or quoted phrase, further required terms, -excluded terms, path: and lang:\n\
filters (including -path: and -lang:). CASE applies to all content terms. Predicates\n\
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
Member pages contain 1..128 rows. Literal queries retain 1..4096 hits, or zero with count-all.\n\
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

/// Legacy commands have fixed defaults and retain their original arities.
fn matching_window(command: &str, controls: &[&str]) -> Result<(bool, bool, u64), Failure> {
    match (command, controls) {
        ("saved-structure" | "saved-structure-text-hex", [skip]) => Ok((true, false, number(skip)?)),
        ("saved-find" | "saved-find-text-hex" | "saved-expression" | "saved-expression-hex", []) =>
            Ok((true, false, 0)),
        ("saved-find-window" | "saved-find-window-hex" | "saved-expression-window"
            | "saved-expression-window-hex" | "saved-structure-window" | "saved-structure-window-hex",
            [case, count, skip]) => Ok((super::repository::case_sensitive(case)?,
                super::repository::count_all(count)?, number(skip)?)),
        _ => Err(Failure::Protocol("DESK_SAVED_COMMAND_SYNTAX")),
    }
}

impl From<SavedRepositoryError> for Failure {
    fn from(error: SavedRepositoryError) -> Self { Self::Saved(SavedDeskError::Saved(error)) }
}
impl From<SavedRelationshipError> for Failure {
    fn from(error: SavedRelationshipError) -> Self { Self::Saved(error.into()) }
}
impl From<SavedImportError> for Failure {
    fn from(error: SavedImportError) -> Self { Self::Saved(error.into()) }
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
            ("saved_path_generation", self.current.as_ref().and_then(|(_, s)| s.accepted_path_generation())),
            ("last_saved_path_attempt", self.current.as_ref().map(|(_, s)| s.last_path_attempt())),
            ("saved_structure_generation", self.current.as_ref().and_then(|(_, s)| s.accepted_structure_generation())),
            ("last_saved_structure_attempt", self.current.as_ref().map(|(_, s)| s.last_structure_attempt())),
            ("saved_index_generation", self.current.as_ref().and_then(|(_, s)| s.index_generation())),
            ("saved_relationship_generation", self.current.as_ref().and_then(|(_, s)| s.relationship_generation())),
            ("saved_import_generation", self.current.as_ref().and_then(|(_, s)| s.import_generation())),
            ("last_saved_import_attempt", self.current.as_ref().map(|(_, s)| s.last_import_attempt())),
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
                let mut candidate = SavedRepositorySession::open(owner, &path, SnapshotLimits::paged(), &mut *canceled)?;
                let response = candidate.info(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.current = Some((attempt, candidate));
                self.expression = None;
                self.last_expression_attempt = 0;
                Ok(response)
            }
            ("saved-info", [token]) => Ok(self.get(number(token)?)?.info(&mut *canceled)?),
            ("saved-members", [token, first, limit]) => Ok(self.get(number(token)?)?.members(count(first)?, count(limit)?, &mut *canceled)?),
            ("saved-paths" | "saved-paths-hex", [token, generation, mode, case, limit, work, text]) => {
                let options = SavedPathOptions {
                    mode: match *mode { "exact" => PathMatchMode::Exact, "prefix" => PathMatchMode::Prefix,
                        "fuzzy" => PathMatchMode::Fuzzy, _ => return Err(Failure::Protocol("DESK_PATH_MODE")) },
                    case: match *case { "sensitive" => PathCase::Sensitive, "unicode-lowercase" => PathCase::UnicodeLowercase,
                        _ => return Err(Failure::Protocol("DESK_PATH_CASE")) },
                    max_results: count(limit)?, max_work_units: number(work)?,
                };
                let decoded;
                let needle = if command == "saved-paths-hex" {
                    decoded = hex(text, MAX_SAVED_PATH_QUERY_BYTES)?; decoded.as_slice()
                } else { text.as_bytes() };
                Ok(self.get(number(token)?)?.path_search(number(generation)?, needle, options, &mut *canceled)?)
            }
            ("saved-path-page", [token, generation, first, limit]) =>
                Ok(self.get(number(token)?)?.path_results(number(generation)?, count(first)?, count(limit)?, &mut *canceled)?),
            ("saved-path-hit", [token, generation, hit]) => {
                let saved = self.get(number(token)?)?;
                let opened = saved.open_path_hit_desk(desk, expected, attempt,
                    number(generation)?, number(hit)?, &mut *canceled)?;
                Ok(saved.desk_open_response(desk, &opened)?)
            }
            ("saved-path-clear", [token, generation]) =>
                Ok(self.get(number(token)?)?.clear_paths(number(generation)?, &mut *canceled)?),
            ("saved-structure" | "saved-structure-text-hex" | "saved-structure-window" | "saved-structure-window-hex",
                [token, generation, mode, name_mode, limit, bytes, controls @ .., text]) => {
                let (case_sensitive, count_all, skip) = matching_window(command, controls)?;
                let mode = match *mode { "symbols" => StructureMode::Symbols,
                    "headings" => StructureMode::Headings, "occurrences" => StructureMode::Occurrences,
                    _ => return Err(Failure::Protocol("DESK_STRUCTURE_MODE")) };
                let name_mode = match *name_mode { "exact" => SymbolNameMode::Exact,
                    "prefix" => SymbolNameMode::Prefix, "contains" => SymbolNameMode::Contains,
                    _ => return Err(Failure::Protocol("DESK_STRUCTURE_NAME_MODE")) };
                let decoded;
                let needle = if command.ends_with("-hex") {
                    decoded = String::from_utf8(hex(text, fcb::search::snapshot_structure::MAX_STRUCTURE_NEEDLE_BYTES)?)
                        .map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                Ok(self.get(number(token)?)?.structure_search_matching_page(number(generation)?, mode, name_mode,
                    needle, count(limit)?, number(bytes)?, case_sensitive, count_all, skip, &mut *canceled)?)
            }
            ("saved-structure-page", [token, generation, first, limit]) =>
                Ok(self.get(number(token)?)?.structure_results(number(generation)?, count(first)?, count(limit)?, &mut *canceled)?),
            ("saved-structure-hit", [token, generation, hit]) => {
                let saved = self.get(number(token)?)?;
                let opened = saved.open_structure_hit_desk(desk, expected, attempt,
                    number(generation)?, number(hit)?, &mut *canceled)?;
                Ok(saved.desk_open_response(desk, &opened)?)
            }
            ("saved-structure-clear", [token, generation]) =>
                Ok(self.get(number(token)?)?.clear_structure(number(generation)?, &mut *canceled)?),
            ("saved-links-build", [token, generation, limit, bytes]) =>
                Ok(self.get(number(token)?)?.prepare_relationships(number(generation)?, count(limit)?, number(bytes)?, &mut *canceled)?),
            ("saved-links-page", [token, generation, first, limit]) =>
                Ok(self.get(number(token)?)?.relationship_page(number(generation)?, None, RelationshipDirection::All,
                    count(first)?, count(limit)?, &mut *canceled)?),
            ("saved-links-from" | "saved-backlinks", [token, generation, member, first, limit]) => {
                let direction = if command == "saved-links-from" { RelationshipDirection::Outgoing }
                    else { RelationshipDirection::Incoming };
                Ok(self.get(number(token)?)?.relationship_page(number(generation)?, Some(count(member)?), direction,
                    count(first)?, count(limit)?, &mut *canceled)?)
            }
            ("saved-link-source" | "saved-link-target", [token, generation, link]) => {
                let side = if command == "saved-link-source" { RelationshipSide::Evidence } else { RelationshipSide::Target };
                let saved = self.get(number(token)?)?;
                let opened = saved.open_relationship_desk(desk, expected, attempt,
                    number(generation)?, number(link)?, side, &mut *canceled)?;
                Ok(saved.desk_open_response(desk, &opened)?)
            }
            ("saved-links-clear", [token, generation]) =>
                Ok(self.get(number(token)?)?.clear_relationships(number(generation)?, &mut *canceled)?),
            ("saved-imports-build", [token, generation, limit, bytes]) =>
                Ok(self.get(number(token)?)?.prepare_imports(number(generation)?, count(limit)?, number(bytes)?, &mut *canceled)?),
            ("saved-imports-page", [token, generation, first, limit]) =>
                Ok(self.get(number(token)?)?.import_page(number(generation)?, None, SavedImportDirection::All,
                    count(first)?, count(limit)?, &mut *canceled)?),
            ("saved-imports-from" | "saved-importers", [token, generation, member, first, limit]) => {
                let direction = if command == "saved-imports-from" { SavedImportDirection::Outgoing }
                    else { SavedImportDirection::Incoming };
                Ok(self.get(number(token)?)?.import_page(number(generation)?, Some(count(member)?), direction,
                    count(first)?, count(limit)?, &mut *canceled)?)
            }
            ("saved-import-source" | "saved-import-target", [token, generation, link]) => {
                let side = if command == "saved-import-source" { SavedImportSide::Evidence } else { SavedImportSide::Target };
                let saved = self.get(number(token)?)?;
                let opened = saved.open_import_desk(desk, expected, attempt,
                    number(generation)?, number(link)?, side, &mut *canceled)?;
                Ok(saved.desk_open_response(desk, &opened)?)
            }
            ("saved-imports-clear", [token, generation]) =>
                Ok(self.get(number(token)?)?.clear_imports(number(generation)?, &mut *canceled)?),
            ("saved-find" | "saved-find-text-hex" | "saved-find-window" | "saved-find-window-hex",
                [token, generation, limit, controls @ .., text]) => {
                let (case_sensitive, count_all, skip) = matching_window(command, controls)?;
                let decoded;
                let needle = if command.ends_with("-hex") {
                    decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                Ok(self.get(number(token)?)?.search_matching_page(number(generation)?, needle, count(limit)?,
                    case_sensitive, count_all, skip, &mut *canceled)?)
            }
            ("saved-expression" | "saved-expression-hex" | "saved-expression-window" | "saved-expression-window-hex",
                [token, generation, limit, bytes, controls @ .., text]) => {
                let (case_sensitive, count_all, skip) = matching_window(command, controls)?;
                let token = number(token)?; self.get(token)?;
                let generation = number(generation)?;
                if generation == 0 || generation <= self.last_expression_attempt {
                    return Err(SavedDeskError::from(ExpressionError::StaleQuery).into());
                }
                self.last_expression_attempt = generation;
                let options = SavedExpressionOptions { max_hits: count(limit)?, max_scan_bytes: number(bytes)? };
                let decoded;
                let query = if command.ends_with("-hex") {
                    decoded = String::from_utf8(hex(text, fcb::search::MAX_QUERY_LEN)?)
                        .map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { text };
                let saved = self.get(token)?;
                let candidate = SavedExpression::prepare_matching_page(saved, generation, query, options,
                    case_sensitive, count_all, skip, &mut *canceled)?;
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
