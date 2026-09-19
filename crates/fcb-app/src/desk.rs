#![forbid(unsafe_code)]

//! Explicit persistent stdio reading session. This is a real headless consumer
//! of host::desk, not a separate source/parser/navigation engine. Input frames
//! are bounded TAB-separated UTF-8 with an LF terminator; native paths can use
//! hex. Output is one complete versioned JSON object per request. No shell,
//! source writes, implicit source stdin, asynchronous runtime or GUI is started.

use std::{ffi::OsString, io::{self, BufReader, Read, Write}, path::{Path, PathBuf}};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::search::{LineNumber, ReadingTarget, ReadingWindowOptions, ResourceBudget};
use crate::{EXIT_OK, EXIT_ERROR, EXIT_PARTIAL, EXIT_CANCELED, owner, allocation};
use crate::host::{HostResponse, desk::{DeskSession, DeskSessionError, DeskLimits, DeskCommand, DeskError}};
use crate::output::Output;

const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_REQUESTS: u64 = 65_536;
const MAX_FRAME_CALLS: usize = MAX_FRAME_BYTES * 2;
const HELP: &str = "fcb desk --stdio\n\
Retain up to 8 reading panes with exact captures, history and session bookmarks.\n\
Input: EXPECTED_REV<TAB>COMMAND<TAB>ARG...<LF>. All numbers are unsigned decimal.\n\
Start at revision 0; use accepted_revision from each response for the next request.\n\
Mutations use the response request number as their new revision; reads do not.\n\
Every response is one fcb.desk-stdio/1 JSON object with result or error.\n\
Commands (arguments separated by literal TABs, not spaces):\n\
  state | quit | back | forward | escape | clear-history\n\
  open PATH | open-hex UNIX_PATH_BYTES_HEX\n\
  view PANE | read PANE OFFSET BYTES LINES | line PANE LINE\n\
  focus PANE | pin PANE | unpin PANE | duplicate PANE | close PANE\n\
  scroll PANE OFFSET | select PANE START END | copy PANE\n\
  find PANE QUERY_GEN LIMIT SCAN_BYTES TEXT\n\
  find-text-hex PANE QUERY_GEN LIMIT SCAN_BYTES UTF8_TEXT_HEX\n\
  hit PANE QUERY_GEN ZERO_BASED_HIT | clear-query QUERY_GEN\n\
  bookmark PANE LABEL | recall BOOKMARK | forget BOOKMARK\n\
Copy returns original bytes as hex; it does not write the clipboard.\n\
open reads one regular file (max 4 MiB), never a directory or implicit stdin.\n\
Later operations use retained bytes even after a live file changes.\n\
UTF-8/BOM UTF-16 windows use the shared reader. Search is exact literal text.\n\
State and bookmarks are session-only; quitting releases them, not a disk save.\n\
No quotes/escapes/shell expansion. Hex permits tabs, newlines and non-UTF-8 paths.\n\
Frames need LF; partial EOF, overlong frames and output failures stop the session.\n\
Ordinary command errors preserve the session; responses expose accepted state.\n\
";

#[derive(Debug)]
enum Failure { Command(DeskSessionError), Protocol(&'static str), Io, Canceled }
impl From<DeskSessionError> for Failure { fn from(e: DeskSessionError) -> Self { Self::Command(e) } }
impl From<DeskError> for Failure { fn from(e: DeskError) -> Self { Self::Command(e.into()) } }
impl Failure {
    fn code(&self) -> String {
        match self { Self::Command(e) => e.to_string(), Self::Protocol(code) => (*code).into(),
            Self::Io => "DESK_INPUT_IO".into(), Self::Canceled => "DESK_CANCELED".into() }
    }
    fn canceled(&self) -> bool { matches!(self, Self::Canceled) || matches!(self, Self::Command(e) if e.is_canceled()) }
}

pub(crate) fn run(arguments: &[OsString], stdin: &mut impl Read, stdout: &mut impl Write,
    stderr: &mut impl Write, mut canceled: impl FnMut() -> bool) -> u8 {
    if arguments.is_empty() || (arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h")) {
        return if stdout.write_all(HELP.as_bytes()).is_ok() && stdout.flush().is_ok() { EXIT_OK } else { EXIT_ERROR };
    }
    if arguments.len() != 1 || arguments[0] != "--stdio" {
        let _ = stderr.write_all(b"DESK_ARGUMENTS: use fcb desk --stdio or fcb desk --help\n"); return EXIT_ERROR;
    }
    let mut session = match DeskSession::new(owner(), DeskLimits::default()) {
        Ok(s) => s, Err(_) => { let _ = stderr.write_all(b"DESK_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    // One reusable encoding reservation and one bounded framing allocation. The
    // session has its own explicit source/reader budget, not a hidden global.
    let budget = match ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)) {
        Ok(b) => b, Err(_) => return EXIT_ERROR,
    };
    let _frame_lease = match budget.try_reserve_managed(owner(), allocation(1), ByteLength::new((4 * MAX_FRAME_BYTES) as u64)) {
        Ok(lease) => lease, Err(_) => return EXIT_ERROR,
    };
    let mut frame = Vec::new();
    if frame.try_reserve_exact(MAX_FRAME_BYTES).is_err() || frame.capacity() > MAX_FRAME_BYTES { return EXIT_ERROR; }
    let mut input = BufReader::with_capacity(4096, stdin);
    let mut output = match Output::new(owner(), 8 * 1024 * 1024, &budget, allocation(2)) {
        Ok(out) => out, Err(_) => return EXIT_ERROR,
    };
    let mut aggregate = EXIT_OK;
    for request in 1..=MAX_REQUESTS {
        let read = read_frame(&mut input, &mut frame, &mut canceled);
        if matches!(read, Ok(false)) { return aggregate; }
        let fatal = read.is_err();
        let result = read.and_then(|_| {
            let text = std::str::from_utf8(&frame).map_err(|_| Failure::Protocol("DESK_FRAME_UTF8"))?;
            execute(&mut session, request, text, &mut canceled)
        });
        let (exit, quit) = match &result {
            Ok((reply, quit)) => (reply.exit_code(), *quit),
            Err(error) => (if error.canceled() { EXIT_CANCELED } else { EXIT_ERROR }, false),
        };
        if exit == EXIT_ERROR { aggregate = EXIT_ERROR; }
        else if exit == EXIT_PARTIAL && aggregate == EXIT_OK { aggregate = EXIT_PARTIAL; }
        output.clear();
        let encoded = (|| {
            output.literal("{\"schema\":\"fcb.desk-stdio/1\",\"request\":")?; output.integer(request)?;
            output.literal(",\"exit_code\":")?; output.integer(exit as u64)?;
            output.literal(",\"accepted_revision\":")?; output.integer(session.model().revision())?;
            output.literal(",\"last_attempt\":")?; output.integer(session.model().last_attempt())?;
            output.literal(",\"accepted_query_generation\":")?;
            match session.accepted_query() { Some(n) => output.integer(n)?, None => output.literal("null")? }
            match &result {
                Ok((reply, _)) => { output.literal(",\"result\":")?; output.literal(reply.as_str().trim_end())?; }
                Err(error) => {
                    output.literal(",\"error\":{\"code\":")?; output.quoted(&error.code())?;
                    output.literal(",\"next_action\":\"Use the accepted revision; do not replay an accepted mutation.\"}")?;
                }
            }
            output.literal("}\n")
        })();
        // Once delivery starts, failure cannot be repaired by a second JSON
        // object. Never execute the next request after a broken/truncated write.
        let mut delivery_canceled = false;
        let mut stop = || {
            let stopped = exit != EXIT_CANCELED && canceled(); delivery_canceled |= stopped; stopped
        };
        if encoded.is_err() || output.deliver(stdout, 4096, &mut stop).is_err() || stdout.flush().is_err() {
            let _ = stderr.write_all(b"DESK_OUTPUT_INTERRUPTED: response incomplete; session stopped\n");
            return if delivery_canceled { EXIT_CANCELED } else { EXIT_ERROR };
        }
        if exit == EXIT_CANCELED { return EXIT_CANCELED; }
        if fatal { return EXIT_ERROR; }
        if quit { return aggregate; }
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
    canceled: &mut impl FnMut() -> bool) -> Result<(HostResponse, bool), Failure> {
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
    let change = match (command, args) {
        ("state" | "quit", []) => return Ok((session.state(&mut *canceled)?, command == "quit")),
        ("open", [path]) => { session.open_file(expected, attempt, Path::new(path), &mut *canceled)?; None }
        ("open-hex", [path]) => { session.open_file(expected, attempt, &raw_path(path)?, &mut *canceled)?; None }
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
