#![forbid(unsafe_code)]

//! Explicit persistent stdio reading session. This is a real headless consumer
//! of host::desk, not a separate source/parser/navigation engine. Input frames
//! are bounded TAB-separated UTF-8 with an LF terminator; native paths can use
//! hex. Output is one complete versioned JSON object per request. No shell,
//! source writes, implicit source stdin, asynchronous runtime or GUI is started.
//! Checkpoint files are created only by an explicit save with a disclosure cap.

mod repository;
use repository::RepositoryCommands;
mod document;
use document::DocumentCommands;

use std::{ffi::OsString, io::{self, BufReader, Read, Write}, path::{Path, PathBuf}};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::search::{LineNumber, ReadingTarget, ReadingWindowOptions, ResourceBudget};
use crate::{EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED, owner, allocation};
use crate::host::{HostResponse, desk::{DeskSession, DeskSessionError, DeskLimits, DeskCommand, DeskError}};
use crate::host::desk::persistence::{CheckpointIoError, CheckpointSaveEffect};
use crate::output::Output;

const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_REQUESTS: u64 = 65_536;
const MAX_FRAME_CALLS: usize = MAX_FRAME_BYTES * 2;
const HELP: &str = "fcb desk --stdio\n\
Retain up to 8 reading panes with exact captures, history and bookmarks.\n\
Input: EXPECTED_REV<TAB>COMMAND<TAB>ARG...<LF>. Counters are unsigned decimal.\n\
Start at revision 0; use accepted_revision from each response for the next request.\n\
Mutations use the response request number as their new revision; reads/save do not.\n\
Every response is one fcb.desk-stdio/1 JSON object with result or error.\n\
Commands (arguments separated by literal TABs, not spaces):\n\
  state | quit | back | forward | escape | clear-history\n\
  open PATH | open-hex UNIX_PATH_BYTES_HEX\n\
  view PANE | read PANE OFFSET BYTES LINES | line PANE LINE\n\
  focus PANE | pin PANE | unpin PANE | duplicate PANE | close PANE\n\
  scroll PANE OFFSET | select PANE START END | copy PANE\n\
  arrange PANE X Y WIDTH HEIGHT (finite logical coordinates)\n\
  find PANE QUERY_GEN LIMIT SCAN_BYTES TEXT\n\
  find-text-hex PANE QUERY_GEN LIMIT SCAN_BYTES UTF8_TEXT_HEX\n\
  hit PANE QUERY_GEN ZERO_BASED_HIT | clear-query QUERY_GEN\n\
  bookmark PANE LABEL | recall BOOKMARK | forget BOOKMARK\n\
  repo-open ROOT | repo-open-hex ROOT_PATH_HEX\n\
  repo-info TOKEN | repo-close TOKEN | repo-clear TOKEN QUERY_GEN\n\
  repo-find TOKEN QUERY_GEN LIMIT SCAN_BYTES TEXT\n\
  repo-find-text-hex TOKEN QUERY_GEN LIMIT SCAN_BYTES UTF8_TEXT_HEX\n\
  repo-page TOKEN QUERY_GEN OFFSET LIMIT | repo-hit TOKEN QUERY_GEN HIT_ID\n\
  save NEW_CHECKPOINT MAX_SOURCE_BYTES | save-hex NEW_PATH_HEX MAX_SOURCE_BYTES\n\
  restore CHECKPOINT | restore-hex PATH_HEX\n\
Copy returns original bytes as hex; it does not write the clipboard.\n\
open reads one regular file (max 4 MiB), never a directory or implicit stdin.\n\
Later operations use retained bytes even after a live file changes.\n\
UTF-8/BOM UTF-16 windows use the shared reader. Search is exact literal text.\n\
Save includes ALL retained sources and personal labels, unencrypted, up to the\n\
explicit source-byte disclosure cap. Existing files are NEVER overwritten.\n\
Restore replaces the desk only after validation, without reopening source paths.\n\
Restored pane/bookmark/source IDs are fresh; read them from the returned state.\n\
Search results are not restored. No autosave: quitting releases unsaved changes.\n\
Repository tokens come from responses; every root replacement gets a new token.\n\
Repository query generations are separate from pane-local query generations.\n\
repo-open freezes up to 4096 catalog entries; repo-find explicitly reads sources\n\
(up to 1 MiB each). Coverage and truncation remain explicit in query responses.\n\
repo-hit uses retained bytes and changes the desk revision; other repo commands\n\
do not. Detaching/replacing the root preserves already opened desk captures.\n\
Save includes activated desk captures, not all search hits or a live root grant.\n\
No quotes/escapes/shell expansion. Hex permits tabs, newlines and non-UTF-8 paths.\n\
Frames need LF; partial EOF, overlong frames and output failures stop the session.\n\
A save receipt reports filesystem effects even on failure. Inspect any created\n\
destination before retrying; keep earlier checkpoints. Transport is not rollback.\n\
Ordinary command errors preserve the session; responses expose accepted state.\n\
";

#[derive(Debug)]
enum Failure { Command(DeskSessionError), Checkpoint(CheckpointIoError), Repository(repository::DeskRepositoryError),
    Document(document::DeskDocumentError), Protocol(&'static str), Io, Canceled }
impl From<DeskSessionError> for Failure { fn from(e: DeskSessionError) -> Self { Self::Command(e) } }
impl From<DeskError> for Failure { fn from(e: DeskError) -> Self { Self::Command(e.into()) } }
impl From<CheckpointIoError> for Failure { fn from(e: CheckpointIoError) -> Self { Self::Checkpoint(e) } }
impl From<repository::DeskRepositoryError> for Failure { fn from(e: repository::DeskRepositoryError) -> Self { Self::Repository(e) } }
impl From<document::DeskDocumentError> for Failure { fn from(e: document::DeskDocumentError) -> Self { Self::Document(e) } }
impl Failure {
    fn code(&self) -> String {
        match self { Self::Command(e) => e.to_string(), Self::Checkpoint(e) => e.code(), Self::Repository(e) => e.to_string(),
            Self::Document(e) => e.to_string(), Self::Protocol(code) => (*code).into(),
            Self::Io => "DESK_INPUT_IO".into(), Self::Canceled => "DESK_CANCELED".into() }
    }
    fn canceled(&self) -> bool {
        match self { Self::Canceled => true, Self::Command(e) => e.is_canceled(),
            Self::Checkpoint(e) => e.is_canceled(), Self::Repository(e) => e.is_canceled(),
            Self::Document(e) => e.is_canceled(), _ => false }
    }
}

pub(crate) fn run(arguments: &[OsString], stdin: &mut impl Read, stdout: &mut impl Write,
    stderr: &mut impl Write, mut canceled: impl FnMut() -> bool) -> u8 {
    if arguments.is_empty() || (arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h")) {
        return if stdout.write_all(HELP.as_bytes()).is_ok()
            && stdout.write_all(repository::WORK_HELP.as_bytes()).is_ok()
            && stdout.write_all(document::DOCUMENT_HELP.as_bytes()).is_ok()
            && stdout.flush().is_ok() { EXIT_OK } else { EXIT_ERROR };
    }
    if arguments.len() != 1 || arguments[0] != "--stdio" {
        let _ = stderr.write_all(b"DESK_ARGUMENTS: use fcb desk --stdio or fcb desk --help\n"); return EXIT_ERROR;
    }
    let mut session = match DeskSession::new(owner(), DeskLimits::default()) {
        Ok(s) => s, Err(_) => { let _ = stderr.write_all(b"DESK_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    // One reusable encoding reservation and bounded framing/layout descriptors.
    // Each actual layout and capture is admitted by the desk's shared budget.
    let budget = match ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)) {
        Ok(b) => b, Err(_) => return EXIT_ERROR,
    };
    let _frame_lease = match budget.try_reserve_managed(owner(), allocation(1),
        ByteLength::new((4 * MAX_FRAME_BYTES + std::mem::size_of::<DocumentCommands>()) as u64)) {
        Ok(lease) => lease, Err(_) => return EXIT_ERROR,
    };
    let mut frame = Vec::new();
    if frame.try_reserve_exact(MAX_FRAME_BYTES).is_err() || frame.capacity() > MAX_FRAME_BYTES { return EXIT_ERROR; }
    let mut input = BufReader::with_capacity(4096, stdin);
    let mut output = match Output::new(owner(), 8 * 1024 * 1024, &budget, allocation(2)) {
        Ok(out) => out, Err(_) => return EXIT_ERROR,
    };
    let mut repository = RepositoryCommands::new();
    let mut documents = DocumentCommands::new();
    let mut aggregate = EXIT_OK;
    for request in 1..=MAX_REQUESTS {
        repository.start_request();
        let read = read_frame(&mut input, &mut frame, &mut canceled);
        if matches!(read, Ok(false)) { return repository.session_exit(aggregate); }
        let fatal = read.is_err();
        let mut effect = CheckpointSaveEffect::None;
        let result = read.and_then(|_| {
            let text = std::str::from_utf8(&frame).map_err(|_| Failure::Protocol("DESK_FRAME_UTF8"))?;
            execute(&mut session, request, text, &mut canceled, &mut effect, &mut repository, &mut documents)
        });
        // Reconcile after ANY accepted navigation, including one whose response
        // encoding failed. Obsolete derived layouts never outlive a pane here.
        documents.retain_current(&session);
        let (exit, quit) = match &result {
            Ok((reply, quit)) => (reply.exit_code(), *quit),
            Err(error) => (if error.canceled() { EXIT_CANCELED } else { EXIT_ERROR }, false),
        };
        let exit = if quit { repository.session_exit(exit) } else { exit };
        if exit == EXIT_ERROR { aggregate = EXIT_ERROR; }
        else if exit == EXIT_PARTIAL && aggregate == EXIT_OK && !repository.is_progress_reply() { aggregate = EXIT_PARTIAL; }
        output.clear();
        let encoded = (|| {
            output.literal("{\"schema\":\"fcb.desk-stdio/1\",\"request\":")?; output.integer(request)?;
            output.literal(",\"exit_code\":")?; output.integer(exit as u64)?;
            output.literal(",\"accepted_revision\":")?; output.integer(session.model().revision())?;
            output.literal(",\"last_attempt\":")?; output.integer(session.model().last_attempt())?;
            output.literal(",\"effect\":")?; output.quoted(effect.code())?;
            output.literal(",\"accepted_query_generation\":")?;
            match session.accepted_query() { Some(n) => output.integer(n)?, None => output.literal("null")? }
            output.literal(",\"repository_token\":")?;
            match repository.token() { Some(n) => output.integer(n)?, None => output.literal("null")? }
            output.literal(",\"repository_query_generation\":")?;
            match repository.query() { Some(n) => output.integer(n)?, None => output.literal("null")? }
            repository.encode_work(&mut output)?;
            documents.encode_state(&mut output)?;
            match &result {
                Ok((reply, _)) => { output.literal(",\"result\":")?; output.literal(reply.as_str().trim_end())?; }
                Err(error) => {
                    output.literal(",\"error\":{\"code\":")?; output.quoted(&error.code())?;
                    output.literal(",\"next_action\":\"Use the accepted revision; inspect any created destination before retrying.\"}")?;
                }
            }
            output.literal("}\n")
        })();
        // Once delivery starts, failure cannot be repaired by a second JSON
        // object. A save that created a file MUST retain its final effect receipt
        // even when cancellation races delivery; transport is never rollback.
        let mut delivery_canceled = false;
        let mut stop = || {
            let stopped = effect == CheckpointSaveEffect::None && exit != EXIT_CANCELED && canceled();
            delivery_canceled |= stopped; stopped
        };
        if encoded.is_err() || output.deliver(stdout, 4096, &mut stop).is_err() || stdout.flush().is_err() {
            let _ = writeln!(stderr, "DESK_OUTPUT_INTERRUPTED: response incomplete; session stopped; effect={}", effect.code());
            return if delivery_canceled { EXIT_CANCELED } else { EXIT_ERROR };
        }
        if exit == EXIT_CANCELED { return EXIT_CANCELED; }
        if fatal { return EXIT_ERROR; }
        if quit { return repository.session_exit(aggregate); }
    }
    let _ = stderr.write_all(b"DESK_REQUEST_LIMIT: session stopped\n"); EXIT_PARTIAL
}

fn read_frame(input: &mut impl Read, frame: &mut Vec<u8>, canceled: &mut impl FnMut() -> bool) -> Result<bool, Failure> {
    frame.clear();
    let mut byte = [0u8; 1];
    for _ in 0..MAX_FRAME_CALLS {
        if canceled() { return Err(Failure::Canceled); }
        match input.read(&mut byte) {
            Ok(0) if frame.is_empty() => return Ok(false),
            Ok(0) => return Err(Failure::Protocol("DESK_FRAME_INCOMPLETE")),
            Ok(1) if byte[0] == b'\n' => {
                if frame.last() == Some(&b'\r') { frame.pop(); }
                return Ok(true);
            }
            Ok(1) => {
                if frame.len() == MAX_FRAME_BYTES { return Err(Failure::Protocol("DESK_FRAME_LIMIT")); }
                frame.push(byte[0]);
            }
            Ok(_) => return Err(Failure::Io),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Failure::Io),
        }
    }
    Err(Failure::Protocol("DESK_FRAME_CALL_LIMIT"))
}

fn number(text: &str) -> Result<u64, Failure> {
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) { return Err(Failure::Protocol("DESK_INVALID_NUMBER")); }
    text.bytes().try_fold(0u64, |n, b| {
        if !b.is_ascii_digit() { return Err(Failure::Protocol("DESK_INVALID_NUMBER")); }
        n.checked_mul(10).and_then(|n| n.checked_add((b - b'0') as u64)).ok_or(Failure::Protocol("DESK_INVALID_NUMBER"))
    })
}
fn count(text: &str) -> Result<usize, Failure> {
    usize::try_from(number(text)?).map_err(|_| Failure::Protocol("DESK_INVALID_NUMBER"))
}
fn coordinate(text: &str) -> Result<f32, Failure> {
    if text.len() > 64 { return Err(Failure::Protocol("DESK_INVALID_COORDINATE")); }
    text.parse::<f32>().ok().filter(|n| n.is_finite()).ok_or(Failure::Protocol("DESK_INVALID_COORDINATE"))
}
fn hex(text: &str, max: usize) -> Result<Vec<u8>, Failure> {
    if text.is_empty() || text.len() % 2 != 0 || text.len() / 2 > max { return Err(Failure::Protocol("DESK_INVALID_HEX")); }
    let mut bytes = Vec::new(); bytes.try_reserve_exact(text.len() / 2).map_err(|_| Failure::Protocol("DESK_FRAME_RESOURCE"))?;
    let nibble = |b| match b { b'0'..=b'9' => Ok(b - b'0'), b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10), _ => Err(Failure::Protocol("DESK_INVALID_HEX")) };
    for pair in text.as_bytes().chunks_exact(2) { bytes.push(nibble(pair[0])? * 16 + nibble(pair[1])?); }
    Ok(bytes)
}
fn raw_path(text: &str) -> Result<PathBuf, Failure> {
    #[cfg(unix)] { use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(OsString::from_vec(hex(text, 16_384)?))) }
    #[cfg(not(unix))] { let _ = text; Err(Failure::Protocol("DESK_NATIVE_PATH_UNSUPPORTED")) }
}
fn execute(session: &mut DeskSession, attempt: u64, frame: &str,
    canceled: &mut impl FnMut() -> bool, effect: &mut CheckpointSaveEffect,
    repository: &mut RepositoryCommands, documents: &mut DocumentCommands) -> Result<(HostResponse, bool), Failure> {
    // Fixed field array: a hostile frame cannot allocate an unbounded token vector.
    let mut fields = [""; 8]; let mut used = 0;
    for field in frame.split('\t') {
        if used == fields.len() { return Err(Failure::Protocol("DESK_FIELD_LIMIT")); }
        fields[used] = field; used += 1;
    }
    if used < 2 { return Err(Failure::Protocol("DESK_FRAME_SYNTAX")); }
    let expected = number(fields[0])?;
    if expected != session.model().revision() { return Err(DeskError::StaleRevision.into()); }
    let args = &fields[2..used];
    let command = fields[1];
    if command.starts_with("repo-") {
        return Ok((repository.execute(session, expected, attempt, command, args, &mut *canceled)?, false));
    }
    if command.starts_with("doc-") {
        return Ok((documents.execute(session, expected, attempt, command, args, &mut *canceled)?, false));
    }
    let change = match (command, args) {
        ("state" | "quit", []) => return Ok((session.state(&mut *canceled)?, command == "quit")),
        ("open", [path]) => { session.open_file(expected, attempt, Path::new(path), &mut *canceled)?; None }
        ("open-hex", [path]) => { session.open_file(expected, attempt, &raw_path(path)?, &mut *canceled)?; None }
        ("save" | "save-hex", [path, limit]) => {
            let path = if command == "save-hex" { raw_path(path)? } else { PathBuf::from(*path) };
            let outcome = session.save_checkpoint(expected, &path, number(limit)?, &mut *canceled);
            *effect = outcome.effect();
            return Ok((session.checkpoint_save_response(&outcome)?, false));
        }
        ("restore" | "restore-hex", [path]) => {
            let path = if command == "restore-hex" { raw_path(path)? } else { PathBuf::from(*path) };
            session.restore_checkpoint_file(expected, attempt, &path, &mut *canceled)?; None
        }
        ("view", [p]) => return Ok((session.window(expected, pane(session, p)?, None, ReadingWindowOptions::default(), &mut *canceled)?, false)),
        ("read", [p, offset, bytes, lines]) => return Ok((session.window(expected, pane(session, p)?,
            Some(ReadingTarget::Byte(ByteOffset::new(number(offset)?))),
            ReadingWindowOptions { max_bytes: count(bytes)?, max_lines: count(lines)? }, &mut *canceled)?, false)),
        ("line", [p, line]) => return Ok((session.window(expected, pane(session, p)?,
            Some(ReadingTarget::Line(LineNumber::new(number(line)?).map_err(|_| Failure::Protocol("DESK_INVALID_LINE"))?)),
            ReadingWindowOptions::default(), &mut *canceled)?, false)),
        ("copy", [p]) => return Ok((session.copy_selection(expected, pane(session, p)?, &mut *canceled)?, false)),
        ("find" | "find-text-hex", [p, generation, limit, bytes, text]) => {
            let decoded;
            let needle = if command == "find-text-hex" {
                decoded = String::from_utf8(hex(text, 1024)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                decoded.as_str()
            } else { text };
            return Ok((session.search(expected, pane(session, p)?, number(generation)?, needle, count(limit)?, number(bytes)?, &mut *canceled)?, false));
        }
        ("hit", [p, generation, index]) => { session.activate_hit(expected, attempt, pane(session, p)?, number(generation)?, count(index)?, &mut *canceled)?; None }
        ("clear-query", [generation]) => { session.clear_query(number(generation)?)?; None }
        ("focus", [p]) => Some(DeskCommand::Focus(pane(session, p)?)),
        ("pin" | "unpin", [p]) => Some(DeskCommand::Pin { pane: pane(session, p)?, pinned: command == "pin" }),
        ("duplicate", [p]) => Some(DeskCommand::Duplicate(pane(session, p)?)),
        ("close", [p]) => Some(DeskCommand::Close(pane(session, p)?)),
        ("scroll", [p, offset]) => Some(DeskCommand::Scroll { pane: pane(session, p)?, offset: number(offset)? }),
        ("select", [p, start, end]) => {
            let start = number(start)?; let end = number(end)?;
            let selected = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| Failure::Protocol("DESK_INVALID_RANGE"))?;
            Some(DeskCommand::Navigate { pane: pane(session, p)?, offset: start, selection: Some(selected) })
        }
        ("arrange", [p, x, y, width, height]) => Some(DeskCommand::Arrange { pane: pane(session, p)?,
            position: (coordinate(x)?, coordinate(y)?), size: (coordinate(width)?, coordinate(height)?) }),
        ("bookmark", [p, label]) => Some(DeskCommand::Bookmark { pane: pane(session, p)?, label: (*label).to_owned() }),
        ("recall", [id]) => Some(DeskCommand::RecallBookmark(number(id)?)),
        ("forget", [id]) => Some(DeskCommand::ForgetBookmark(number(id)?)),
        ("back", []) => Some(DeskCommand::Back), ("forward", []) => Some(DeskCommand::Forward),
        ("escape", []) => Some(DeskCommand::Escape), ("clear-history", []) => Some(DeskCommand::ClearHistory),
        _ => return Err(Failure::Protocol("DESK_COMMAND_SYNTAX")),
    };
    if let Some(change) = change { session.apply(expected, attempt, change, &mut *canceled)?; }
    Ok((session.state(&mut *canceled)?, false))
}

fn pane(session: &DeskSession, text: &str) -> Result<crate::host::desk::DeskPaneId, Failure> {
    Ok(session.model().pane_id(number(text)?)?)
}
