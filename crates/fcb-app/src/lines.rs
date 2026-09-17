#![forbid(unsafe_code)]

//! Explicit, bounded line navigation over one named regular file. The selected
//! bytes are retained during the same forward scan that establishes their line
//! numbers; they are never re-read from a possibly changed live source.

use std::{ffi::OsString, fs::Metadata, io::{self, Read, Write}, path::PathBuf};
use fcb::{BrowserSession, ByteLength, ByteOffset, ByteRange};
use fcb::search::{CaptureRequest, ObservedExtent, ResourceBudget};
use fcb::source::{DetectedEncoding, LineNumber};
use fcb::source::line_index::{LineWindowScanner, LineWindowStatus};
use fcb_core::ResourceLease;
use crate::{AppError, SCHEMA, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL, EXIT_ERROR,
    EXIT_CANCELED, MANAGED_BYTES, allocation, file_id, generation, owner, revision};
use crate::args::{self, ArgumentError, Encoding};
use crate::output::{Output, MAX_RESPONSE_BYTES};
use crate::input;

const BUFFER_BYTES: usize = 16 * 1024;
const MAX_READ_CALLS: u64 = 131_072;
const HELP: &str = "Line-number source reading\n\n\
  fcb read-lines FILE [--line N] [--lines N] [--json]\n\
  fcb read FILE --line N [--lines N] [--json]\n\
  fcb open FILE --line N [--lines N] --json\n\
  --bytes N                  Maximum retained selection (default 65536, max 262144)\n\
  --max-scan-bytes N          Prefix scan allowance (default 268435456, max 1 TiB)\n\
  --encoding auto|utf8|utf16le|utf16be\n\
Defaults: line 1, 20 lines. Counts are one-based, canonical unsigned decimals.\n\
CR, LF, and CRLF terminate lines; a final terminator adds no phantom line.\n\
Original terminators and bytes are preserved. A BOM belongs to line 1.\n\
A short final window is complete only after EOF. A budget limit is partial,\n\
not a missing line. No whole-file buffer, index, stdin, workspace scan, or write.\n\
Exit: 0 resolved, 1 missing at EOF, 2 error, 3 budget-limited, 130 canceled.\n\
Implemented-unqualified; independent Rust/RCH verification remains pending.\n";

fn takes_value(arg: &str) -> bool {
    matches!(arg, "--line" | "--lines" | "--offset" | "--bytes" | "--limit" |
        "--encoding" | "--max-scan-bytes" | "--text" | "--raw-hex" | "--query" |
        "--path" | "--max-files" | "--max-file-bytes" | "--max-total-bytes")
}

/// A flag used as another option's value or after `--` is always data.
pub(crate) fn requested(args: &[OsString]) -> bool {
    if args.first().is_some_and(|arg| arg == "read-lines") { return true; }
    if !args.first().is_some_and(|arg| arg == "read" || arg == "open") { return false; }
    let mut cursor = 1;
    while cursor < args.len().min(args::MAX_ARGUMENTS + 1) {
        let arg = &args[cursor]; cursor += 1;
        if arg == "--" { break; }
        if arg == "--line" || arg == "--lines" { return true; }
        if arg.to_str().is_some_and(takes_value) { cursor += 1; }
    }
    false
}

fn json_requested(args: &[OsString]) -> bool {
    let mut cursor = 1;
    while cursor < args.len().min(args::MAX_ARGUMENTS + 1) {
        let arg = &args[cursor]; cursor += 1;
        if arg == "--" { break; }
        if arg == "--json" { return true; }
        if arg.to_str().is_some_and(takes_value) { cursor += 1; }
    }
    false
}

#[derive(Debug)]
struct Request {
    first: LineNumber, count: u64, max_bytes: usize, max_scan: u64,
    encoding: Encoding, path: Option<PathBuf>, json: bool, help: bool,
}

fn parse(argv: &[OsString]) -> Result<Request, ArgumentError> {
    if argv.len() > args::MAX_ARGUMENTS { return Err(ArgumentError::Limit); }
    let mut total = 0usize;
    for arg in argv {
        if arg.len() > args::MAX_SINGLE_ARGUMENT { return Err(ArgumentError::Limit); }
        total = total.checked_add(arg.len()).ok_or(ArgumentError::Limit)?;
        if total > args::MAX_ARGUMENT_BYTES { return Err(ArgumentError::Limit); }
    }
    let mut request = Request { first: LineNumber::new(1).expect("nonzero line"), count: 20,
        max_bytes: 65_536, max_scan: 256 * 1024 * 1024, encoding: Encoding::Auto,
        path: None, json: false, help: false };
    let mut cursor = 1;
    let mut positional = false;
    let mut seen = 0u16;
    while cursor < argv.len() {
        let arg = &argv[cursor]; cursor += 1;
        if !positional && arg == "--" { positional = true; continue; }
        if !positional && arg.to_str().is_some_and(|arg| arg.starts_with('-')) {
            let option = arg.to_str().ok_or(ArgumentError::UnknownOption)?;
            let bit = match option {
                "--json" => 1, "--line" => 2, "--lines" => 4, "--bytes" => 8,
                "--max-scan-bytes" => 16, "--encoding" => 32, "--help" | "-h" => 64,
                _ => return Err(ArgumentError::UnknownOption),
            };
            if seen & bit != 0 { return Err(ArgumentError::DuplicateOption); }
            seen |= bit;
            if option == "--json" { request.json = true; continue; }
            if matches!(option, "--help" | "-h") { request.help = true; continue; }
            let value = argv.get(cursor).and_then(|value| value.to_str()).ok_or(ArgumentError::MissingValue)?;
            cursor += 1;
            match option {
                "--line" => request.first = LineNumber::new(args::decimal(value)?).map_err(|_| ArgumentError::InvalidNumber)?,
                "--lines" => {
                    let count = args::decimal(value)?;
                    if !(1..=4096).contains(&count) { return Err(ArgumentError::Limit); }
                    request.count = count;
                }
                "--bytes" => {
                    let count = args::decimal(value)?;
                    if !(1..=args::MAX_WINDOW_BYTES as u64).contains(&count) { return Err(ArgumentError::Limit); }
                    request.max_bytes = count as usize;
                }
                "--max-scan-bytes" => {
                    let count = args::decimal(value)?;
                    if count > args::MAX_SCAN_BYTES { return Err(ArgumentError::Limit); }
                    request.max_scan = count;
                }
                "--encoding" => request.encoding = match value {
                    "auto" => Encoding::Auto, "utf8" => Encoding::Utf8,
                    "utf16le" => Encoding::Utf16Le, "utf16be" => Encoding::Utf16Be,
                    _ => return Err(ArgumentError::InvalidEncoding),
                },
                _ => return Err(ArgumentError::UnknownOption),
            }
        } else {
            if request.path.is_some() { return Err(ArgumentError::MultipleSources); }
            if arg.is_empty() { return Err(ArgumentError::MissingSource); }
            request.path = Some(PathBuf::from(arg));
        }
    }
    request.first.get().checked_add(request.count - 1).ok_or(ArgumentError::InvalidNumber)?;
    if request.help {
        if request.path.is_some() || seen & !(1 | 64) != 0 { return Err(ArgumentError::IncompatibleOptions); }
    } else if request.path.is_none() { return Err(ArgumentError::MissingSource); }
    Ok(request)
}

pub(crate) fn run(argv: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write,
    mut canceled: impl FnMut() -> bool) -> u8 {
    let json = json_requested(argv);
    let budget = match ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)) {
        Ok(budget) => budget,
        Err(_) => { let _ = stderr.write_all(b"CLI_RESOURCE_DENIED\n"); return EXIT_ERROR; }
    };
    let mut out = match Output::new(owner(), MAX_RESPONSE_BYTES, &budget, allocation(1)) {
        Ok(out) => out,
        Err(_) => { let _ = stderr.write_all(b"CLI_OUTPUT_ADMISSION\n"); return EXIT_ERROR; }
    };
    let result = parse(argv).map_err(AppError::from).and_then(|request| {
        if canceled() { return Err(AppError::Canceled); }
        if request.help {
            if json {
                out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
                out.literal(",\"status\":\"ok\",\"command\":\"read-lines-help\",\"text\":")?;
                out.quoted(HELP)?; out.literal("}\n")?;
            } else { out.literal(HELP)?; }
            return Ok(EXIT_OK);
        }
        if argv.first().is_some_and(|arg| arg == "open") && !request.json { return Err(AppError::GuiUnavailable); }
        execute(&request, &mut out, &budget, &mut canceled)
    });
    let result = if canceled() && result.is_ok() { Err(AppError::Canceled) } else { result };
    let exit = match result {
        Ok(exit) => exit,
        Err(error) => {
            out.clear();
            if crate::encode_error(&mut out, json, error).is_err() {
                let _ = stderr.write_all(b"CLI_ERROR_ENCODING_FAILED\n"); return EXIT_ERROR;
            }
            if error.is_canceled() { EXIT_CANCELED } else { EXIT_ERROR }
        }
    };
    let mut interrupted = false;
    let mut stop = || {
        if exit != EXIT_CANCELED && canceled() { interrupted = true; true } else { false }
    };
    let delivery = if json || matches!(exit, EXIT_OK | EXIT_NO_MATCH | EXIT_PARTIAL) {
        out.deliver(stdout, 4096, &mut stop)
    } else { out.deliver(stderr, 4096, &mut stop) };
    if delivery.is_err() {
        let _ = stderr.write_all(b"CLI_OUTPUT_INTERRUPTED: response delivery incomplete\n");
        return if interrupted { EXIT_CANCELED } else { EXIT_ERROR };
    }
    exit
}

struct Scan {
    cursor: LineWindowScanner,
    encoding: DetectedEncoding,
    selected: Vec<u8>,
    backing_start: Option<u64>,
    bytes_read: u64,
    read_calls: u64,
    stop: &'static str,
    _selected: ResourceLease,
}

fn read(reader: &mut impl Read, buffer: &mut [u8], calls: &mut u64,
    canceled: &mut impl FnMut() -> bool) -> Result<Option<usize>, AppError> {
    if canceled() { return Err(AppError::Canceled); }
    *calls += 1;
    match reader.read(buffer) {
        Ok(count) if count <= buffer.len() => Ok(Some(count)),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
        _ => Err(AppError::Io),
    }
}

/// Keep the four-byte decoder prefix/suffix in the same observed sequence.
/// Eight bytes of rolling history cover a split UTF-16 unit and its prefix.
/// The same bytes can occur in successive input blocks; high-water accounting
/// appends each original byte once without reconstructing source from text.
fn retain(scan: &mut Scan, base: u64, bytes: &[u8], capacity: usize) -> Result<bool, AppError> {
    let Some(start) = scan.cursor.selected_start().map(|offset| offset.get()) else { return Ok(true); };
    let backing_start = *scan.backing_start.get_or_insert(start.saturating_sub(4));
    let append_from = backing_start + scan.selected.len() as u64;
    let available_end = base + bytes.len() as u64;
    let wanted_end = match scan.cursor.status() {
        LineWindowStatus::Resolved { range, .. } => range.end().get().checked_add(4).ok_or(AppError::InvalidRange)?,
        _ => scan.cursor.bytes_scanned(),
    };
    let end = available_end.min(wanted_end);
    if end <= append_from { return Ok(true); }
    if append_from < base { return Err(AppError::InvalidRange); }
    let slice = &bytes[(append_from - base) as usize..(end - base) as usize];
    if slice.len() > capacity - scan.selected.len() { return Ok(false); }
    scan.selected.extend_from_slice(slice);
    Ok(true)
}

fn scan(reader: &mut impl Read, request: &Request, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<Scan, AppError> {
    let capacity = request.max_bytes.checked_add(8).ok_or(AppError::InputLimit)?;
    let selected_lease = budget.try_reserve_managed(owner(), allocation(61), ByteLength::new(capacity as u64))
        .map_err(|_| AppError::Admission)?;
    let _input = budget.try_reserve_managed(owner(), allocation(62), ByteLength::new((BUFFER_BYTES + 8) as u64))
        .map_err(|_| AppError::Admission)?;
    let mut selected = Vec::new();
    selected.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if selected.capacity() > capacity { return Err(AppError::Admission); }
    let mut prefix = [0u8; 3];
    let mut filled = 0usize;
    let mut calls = 0u64;
    let mut eof = false;
    while filled < prefix.len() && (filled as u64) < request.max_scan && calls < MAX_READ_CALLS {
        let count = (prefix.len() - filled).min((request.max_scan - filled as u64).min(3) as usize);
        match read(reader, &mut prefix[filled..filled + count], &mut calls, canceled)? {
            Some(0) => { eof = true; break; }
            Some(count) => filled += count,
            None => {}
        }
    }
    let encoding = match request.encoding {
        Encoding::Auto => fcb::source::detect_encoding(&prefix[..filled]),
        Encoding::Utf8 => DetectedEncoding::Utf8 { has_bom: prefix[..filled].starts_with(&[0xef, 0xbb, 0xbf]) },
        Encoding::Utf16Le => DetectedEncoding::Utf16Le,
        Encoding::Utf16Be => DetectedEncoding::Utf16Be,
    };
    let cursor = LineWindowScanner::new(request.first, request.count, encoding).map_err(|_| AppError::InvalidRange)?;
    let mut result = Scan { cursor, encoding, selected, backing_start: None, bytes_read: filled as u64,
        read_calls: calls, stop: "pending", _selected: selected_lease };
    result.cursor.step(ByteOffset::new(0), &prefix[..filled], filled).map_err(|_| AppError::InvalidRange)?;
    let mut fits = retain(&mut result, 0, &prefix[..filled], capacity)?;
    let mut history = [0u8; 8];
    history[..filled].copy_from_slice(&prefix[..filled]);
    let mut history_len = filled;
    let mut buffer = [0u8; BUFFER_BYTES + 8];
    loop {
        if canceled() { return Err(AppError::Canceled); }
        if eof {
            result.cursor.finish().map_err(|_| AppError::InvalidRange)?;
            // EOF may identify a final line consisting of a malformed UTF-16
            // byte. The rolling history still retains it and its prefix.
            let base = result.bytes_read - history_len as u64;
            fits &= retain(&mut result, base, &history[..history_len], capacity)?;
        }
        if !fits { result.stop = "output-byte-limit"; break; }
        let mut wanted = BUFFER_BYTES as u64;
        match result.cursor.status() {
            LineWindowStatus::Resolved { range, .. } => {
                if range.len().get() > request.max_bytes as u64 { result.stop = "output-byte-limit"; break; }
                let context_end = range.end().get().checked_add(4).ok_or(AppError::InvalidRange)?;
                if eof || result.bytes_read >= context_end { result.stop = "resolved"; break; }
                wanted = context_end - result.bytes_read;
            }
            LineWindowStatus::Missing => { result.stop = "missing"; break; }
            LineWindowStatus::Pending => {}
        }
        if result.bytes_read == request.max_scan { result.stop = "scan-byte-limit"; break; }
        if result.read_calls == MAX_READ_CALLS { result.stop = "read-call-limit"; break; }
        let count = wanted.min(request.max_scan - result.bytes_read).min(BUFFER_BYTES as u64) as usize;
        buffer[..history_len].copy_from_slice(&history[..history_len]);
        let Some(count) = read(reader, &mut buffer[history_len..history_len + count], &mut result.read_calls, canceled)? else { continue; };
        if count == 0 { eof = true; continue; }
        let base = result.bytes_read;
        result.bytes_read += count as u64;
        if !result.cursor.is_finished() {
            result.cursor.step(ByteOffset::new(base), &buffer[history_len..history_len + count], count)
                .map_err(|_| AppError::InvalidRange)?;
        }
        let available = history_len + count;
        fits = retain(&mut result, base - history_len as u64, &buffer[..available], capacity)?;
        history_len = available.min(history.len());
        history[..history_len].copy_from_slice(&buffer[available - history_len..available]);
    }
    if canceled() { return Err(AppError::Canceled); }
    Ok(result)
}

fn same_metadata(left: &Metadata, right: &Metadata) -> bool {
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino() && left.len() == right.len()
            && left.mtime() == right.mtime() && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime() && left.ctime_nsec() == right.ctime_nsec()
    }
    #[cfg(not(unix))] { left.len() == right.len() && left.modified().ok() == right.modified().ok() }
}

fn execute(request: &Request, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let _paths = budget.try_reserve_managed(owner(), allocation(60), ByteLength::new(128 * 1024))
        .map_err(|_| AppError::Admission)?;
    let path = input::absolute(request.path.as_deref().ok_or(AppError::InvalidRange)?)?;
    let (mut file, before) = input::open_regular(&path)?;
    let result = scan(&mut file, request, budget, canceled)?;
    let after = file.metadata().map_err(|_| AppError::Io)?;
    let named = std::fs::symlink_metadata(&path).map_err(|_| AppError::SourceChanged)?;
    if !same_metadata(&before, &after) || !same_metadata(&after, &named) { return Err(AppError::SourceChanged); }
    if canceled() { return Err(AppError::Canceled); }
    let complete = matches!(result.stop, "resolved" | "missing");
    if request.json {
        out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"status\":\"ok\",\"command\":\"read-lines\",\"scope\":\"named-file\",\"identity_scope\":\"response-local\",\"file_id\":\"1\",\"revision\":\"1\",\"path\":")?;
        out.path(&path)?;
        out.literal(",\"consistency\":\"unchanged-metadata-not-atomic\",\"state\":")?; out.quoted(result.stop)?;
        out.literal(",\"line_window_complete\":")?; out.boolean(complete)?;
        out.literal(",\"input_complete\":")?; out.boolean(result.cursor.reached_eof())?;
        out.literal(",\"requested_first_line\":")?; out.integer(request.first.get())?;
        out.literal(",\"requested_lines\":")?; out.integer(request.count)?;
        out.literal(",\"observed_length\":")?; out.integer(before.len())?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(result.bytes_read)?;
        out.literal(",\"scanned_input_bytes\":")?; out.integer(result.cursor.bytes_scanned())?;
        out.literal(",\"lines_seen\":")?; out.integer(result.cursor.lines_seen())?;
        out.literal(",\"read_calls\":")?; out.integer(result.read_calls)?;
        out.literal(",\"max_scan_bytes\":")?; out.integer(request.max_scan)?;
        out.literal(",\"max_selected_bytes\":")?; out.integer(request.max_bytes as u64)?;
        out.literal(",\"max_read_calls\":")?; out.integer(MAX_READ_CALLS)?;
        out.literal(",\"input_buffer_bytes\":")?; out.integer(BUFFER_BYTES as u64)?;
    } else {
        out.literal("Line navigation: ")?; out.literal(result.stop)?; out.literal("\n")?;
    }
    if result.stop == "resolved" {
        let LineWindowStatus::Resolved { first, last, range } = result.cursor.status() else { return Err(AppError::InvalidRange); };
        let start = result.backing_start.ok_or(AppError::InvalidRange)?;
        let backing = ByteRange::new(ByteOffset::new(start), ByteOffset::new(start + result.selected.len() as u64))
            .map_err(|_| AppError::InvalidRange)?;
        let capture = CaptureRequest::new(file_id(), revision()).map_err(|_| AppError::InvalidRange)?
            .with_range(backing).map_err(|_| AppError::InvalidRange)?;
        let extent = ObservedExtent::from_bytes(capture, ByteLength::new(before.len()), &result.selected, budget, allocation(63))?;
        let view = BrowserSession::new(owner()).open_extent(extent)?;
        let encoding = match result.encoding {
            DetectedEncoding::Utf8 { has_bom } => DetectedEncoding::Utf8 { has_bom: has_bom && range.start().get() == 0 },
            encoding => encoding,
        };
        let text = view.decode(range, encoding, generation(), budget, allocation(64), &mut *canceled)?;
        if request.json {
            out.literal(",\"first_line\":")?; out.integer(first.get())?;
            out.literal(",\"last_line\":")?; out.integer(last.get())?;
            out.literal(",\"original_range\":")?; out.range(range)?;
            out.literal(",\"visible_range\":")?; out.range(text.range())?;
            out.literal(",\"encoding\":")?;
            out.quoted(match encoding { DetectedEncoding::Utf16Le => "utf16le", DetectedEncoding::Utf16Be => "utf16be", _ => "utf8" })?;
            out.literal(",\"text\":")?; out.quoted(text.text())?;
            out.literal(",\"original_hex\":")?; out.hex(view.raw_selection(range)?)?;
            out.literal(",\"has_replacements\":")?; out.boolean(text.has_replacements())?;
        } else {
            out.literal("[lines ")?; out.literal(&first.get().to_string())?; out.literal("..")?;
            out.literal(&last.get().to_string())?; out.literal("; escaped controls]\n")?;
            out.human_text(text.text())?;
            if !text.text().ends_with('\n') { out.literal("\n")?; }
            if text.has_replacements() { out.literal("[malformed text replaced; original bytes retained]\n")?; }
        }
    } else if request.json {
        out.literal(",\"first_line\":null,\"last_line\":null,\"original_range\":null,\"text\":null,\"original_hex\":null")?;
    } else if !complete {
        out.literal("PARTIAL: increase the reported scan/output allowance; no missing-line claim or truncated text was published.\n")?;
    }
    if request.json { out.literal("}\n")?; }
    Ok(if !complete { EXIT_PARTIAL } else if result.stop == "missing" { EXIT_NO_MATCH } else { EXIT_OK })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn argv(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    fn request(first: u64, count: u64) -> Request {
        Request { first: LineNumber::new(first).unwrap(), count, max_bytes: 65_536,
            max_scan: 1024 * 1024, encoding: Encoding::Auto, path: None, json: true, help: false }
    }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)).unwrap() }
    fn exact_bytes(result: &Scan) -> &[u8] {
        let LineWindowStatus::Resolved { range, .. } = result.cursor.status() else { panic!("not resolved") };
        let start = result.backing_start.unwrap();
        &result.selected[(range.start().get() - start) as usize..(range.end().get() - start) as usize]
    }
    fn decoded(result: &Scan, length: usize, budget: &ResourceBudget) -> String {
        let LineWindowStatus::Resolved { range, .. } = result.cursor.status() else { panic!("not resolved") };
        let start = result.backing_start.unwrap();
        let backing = ByteRange::new(ByteOffset::new(start), ByteOffset::new(start + result.selected.len() as u64)).unwrap();
        let request = CaptureRequest::new(file_id(), revision()).unwrap().with_range(backing).unwrap();
        let extent = ObservedExtent::from_bytes(request, ByteLength::new(length as u64), &result.selected, budget, allocation(63)).unwrap();
        BrowserSession::new(owner()).open_extent(extent).unwrap()
            .decode(range, result.encoding, generation(), budget, allocation(64), || false).unwrap().text().to_owned()
    }

    struct ShortReads<'a> { input: Cursor<&'a [u8]>, chunk: usize, interrupt: bool, calls: usize }
    impl Read for ShortReads<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.interrupt && self.calls % 3 == 0 { return Err(io::Error::from(io::ErrorKind::Interrupted)); }
            let count = output.len().min(self.chunk);
            self.input.read(&mut output[..count])
        }
    }

    #[test]
    fn aliases_respect_argument_values_delimiters_and_raw_paths() {
        assert!(requested(&argv(&["read-lines", "file"])));
        assert!(requested(&argv(&["read", "file", "--line", "100"])));
        assert!(!requested(&argv(&["read", "--encoding", "--line"])));
        assert!(!requested(&argv(&["read", "--", "--line"])));
        assert!(!requested(&argv(&["search", "file", "--text", "--line"])));
        assert!(!json_requested(&argv(&["read-lines", "file", "--line", "--json"])));
        let parsed = parse(&argv(&["read-lines", "--line", "9007199254740993", "--lines", "1", "--", "--file"])).unwrap();
        assert_eq!(parsed.first.get(), 9_007_199_254_740_993);
        assert_eq!(parsed.path, Some(PathBuf::from("--file")));
    }

    #[test]
    fn invalid_scope_and_numeric_inputs_fail_before_any_file_io() {
        for values in [vec!["read-lines", "file", "--line", "0"],
            vec!["read-lines", "file", "--line", "01"], vec!["read-lines", "file", "--lines", "0"],
            vec!["read-lines", "file", "--line", "18446744073709551615", "--lines", "2"],
            vec!["read-lines", "file", "--offset", "0"], vec!["read-lines", "--stdin"],
            vec!["read-lines", "file", "--workspace"], vec!["read-lines", "file", "--whole-file"],
            vec!["read-lines", "file", "--line", "1", "--line", "2"],
            vec!["read-lines", "file", "--bytes", "262145"],
            vec!["read-lines", "file", "--max-scan-bytes", "1099511627777"]] {
            assert!(parse(&argv(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn short_reads_and_interrupts_preserve_utf16_lines_and_decoder_context() {
        for encoding in [DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
            let input: Vec<u8> = "\u{feff}first\r\n\u{feff}\u{1f980}\u{0a41}\rthird\nend".encode_utf16()
                .flat_map(|unit| if encoding == DetectedEncoding::Utf16Le { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
            for chunk in 1..=9 {
                let budget = budget();
                let mut reader = ShortReads { input: Cursor::new(input.as_slice()), chunk, interrupt: true, calls: 0 };
                let result = scan(&mut reader, &request(2, 1), &budget, &mut || false).unwrap();
                assert_eq!(result.stop, "resolved");
                assert_eq!(decoded(&result, input.len(), &budget), "\u{feff}\u{1f980}\u{0a41}\r");
                assert_eq!(result.encoding, encoding);
                assert!(result.read_calls >= 3);
            }
        }
    }

    #[test]
    fn input_buffer_boundaries_do_not_change_line_numbers_or_selected_bytes() {
        for pad in BUFFER_BYTES - 8..=BUFFER_BYTES + 8 {
            let mut input = vec![b'x'; pad];
            input.extend_from_slice(b"\r\nchosen\r\nnext\nlast");
            let budget = budget();
            let result = scan(&mut Cursor::new(&input), &request(2, 1), &budget, &mut || false).unwrap();
            assert_eq!(result.stop, "resolved");
            assert_eq!(exact_bytes(&result), b"chosen\r\n");
            assert_eq!(decoded(&result, input.len(), &budget), "chosen\r\n");
            assert!(result.selected.len() <= b"chosen\r\n".len() + 8);
        }
    }

    #[test]
    fn a_long_prefix_is_scanned_but_not_retained() {
        let input = format!("{}chosen\nrest", "prefix\n".repeat(50_000));
        let budget = budget();
        let result = scan(&mut Cursor::new(input.as_bytes()), &request(50_001, 1), &budget, &mut || false).unwrap();
        assert_eq!(result.stop, "resolved");
        assert_eq!(exact_bytes(&result), b"chosen\n");
        assert!(result.bytes_read > 300_000);
        assert!(result.selected.len() <= 15);
        assert_eq!(decoded(&result, input.len(), &budget), "chosen\n");
    }

    #[test]
    fn limits_are_not_eof_and_never_publish_truncated_lines() {
        let budget = budget();
        let mut options = request(1, 1); options.max_scan = 0;
        let result = scan(&mut Cursor::new(b"a\n"), &options, &budget, &mut || false).unwrap();
        assert_eq!(result.stop, "scan-byte-limit"); assert_eq!(result.bytes_read, 0);
        assert!(!result.cursor.reached_eof());
        drop(result);
        options.max_scan = 2;
        let result = scan(&mut Cursor::new(b"a\nrest"), &options, &budget, &mut || false).unwrap();
        assert_eq!(result.stop, "scan-byte-limit");
        assert!(!result.cursor.reached_eof());
        drop(result);
        options.max_scan = 100; options.max_bytes = 1;
        let result = scan(&mut Cursor::new(b"larger than admitted\n"), &options, &budget, &mut || false).unwrap();
        assert_eq!(result.stop, "output-byte-limit"); assert!(result.selected.len() <= 9);
    }

    #[test]
    fn real_eof_distinguishes_missing_and_a_short_final_window() {
        for bytes in [b"".as_slice(), b"first\n", b"first\r\n", b"first\r"] {
            let result = scan(&mut Cursor::new(bytes), &request(2, 1), &budget(), &mut || false).unwrap();
            assert_eq!(result.stop, "missing"); assert!(result.cursor.reached_eof());
        }
        let budget = budget();
        let result = scan(&mut Cursor::new(b"first\nlast"), &request(2, 20), &budget, &mut || false).unwrap();
        assert_eq!(result.stop, "resolved"); assert!(result.cursor.reached_eof());
        assert_eq!(exact_bytes(&result), b"last");
        assert_eq!(decoded(&result, 10, &budget), "last");
    }

    #[test]
    fn caller_cancellation_and_read_failures_are_terminal() {
        let error = scan(&mut Cursor::new(b"first\nlast"), &request(2, 1), &budget(), &mut || true).err().unwrap();
        assert_eq!(error, AppError::Canceled);
        struct Fail;
        impl Read for Fail { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::Error::from(io::ErrorKind::PermissionDenied)) } }
        assert_eq!(scan(&mut Fail, &request(1, 1), &budget(), &mut || false).err().unwrap(), AppError::Io);
    }

    #[test]
    fn endlessly_interrupted_reader_has_a_terminal_call_budget() {
        struct Interrupt;
        impl Read for Interrupt { fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::Error::from(io::ErrorKind::Interrupted)) } }
        let result = scan(&mut Interrupt, &request(1, 1), &budget(), &mut || false).unwrap();
        assert_eq!(result.stop, "read-call-limit"); assert_eq!(result.read_calls, MAX_READ_CALLS);
        assert!(!result.cursor.reached_eof());
    }

    #[test]
    fn public_entry_point_publishes_exact_json_and_read_aliases() {
        if !input::NATIVE_FILE_SUPPORTED { return; }
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-line-read-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        file.write_all(b"first\r\nsecond\nlast").unwrap();
        drop(file);
        for command in ["read-lines", "read", "open"] {
            let arguments = vec![OsString::from(command), path.as_os_str().to_owned(),
                "--line".into(), "2".into(), "--lines".into(), "1".into(), "--json".into()];
            let mut output = Vec::new(); let mut errors = Vec::new();
            let exit = crate::run(&arguments, &mut Cursor::new(b"unused stdin"), &mut output, &mut errors, || false);
            assert_eq!(exit, EXIT_OK, "{}", String::from_utf8_lossy(&output));
            assert!(errors.is_empty());
            let json = String::from_utf8(output).unwrap();
            assert!(json.contains("\"state\":\"resolved\""));
            assert!(json.contains("\"first_line\":\"2\""));
            assert!(json.contains("\"original_range\":{\"start\":\"7\",\"end\":\"14\"}"));
            assert!(json.contains("\"text\":\"second\\n\""));
            assert!(json.contains("\"original_hex\":\"7365636f6e640a\""));
        }
        // Only this test's create_new-owned file is removed.
        std::fs::remove_file(path).unwrap();
    }
}
