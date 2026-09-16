#![forbid(unsafe_code)]

//! Process arguments belong to fcb-app, never to library construction.
//! This first CLI lane operates on ONE explicitly named file or bounded stdin.
//! Directory/workspace commands are not silently reinterpreted as file queries.

use std::{ffi::OsString, path::PathBuf};

pub const MAX_ARGUMENTS: usize = 64;
pub const MAX_ARGUMENT_BYTES: usize = 65_536;
pub const MAX_SINGLE_ARGUMENT: usize = 16_384;
pub const MAX_WINDOW_BYTES: usize = 256 * 1024;
pub const MAX_RESULTS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command { Help, Capabilities, Doctor, Inspect, Open, Read, Search, Launch }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding { Auto, Utf8, Utf16Le, Utf16Be }
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Needle { Text(String), Raw(Vec<u8>) }
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Arguments {
    pub command: Command,
    pub json: bool,
    pub file: Option<PathBuf>,
    pub stdin: bool,
    pub offset: u64,
    pub bytes: usize,
    pub limit: usize,
    pub encoding: Encoding,
    pub needle: Option<Needle>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentError { Limit, UnknownCommand, UnknownOption, DuplicateOption, MissingValue,
    MissingSource, MultipleSources, InvalidNumber, InvalidEncoding, InvalidNeedle, IncompatibleOptions }
impl ArgumentError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limit => "CLI_ARGUMENT_LIMIT", Self::UnknownCommand => "CLI_UNKNOWN_COMMAND",
            Self::UnknownOption => "CLI_UNKNOWN_OPTION", Self::DuplicateOption => "CLI_DUPLICATE_OPTION",
            Self::MissingValue => "CLI_MISSING_VALUE", Self::MissingSource => "CLI_MISSING_SOURCE",
            Self::MultipleSources => "CLI_MULTIPLE_SOURCES", Self::InvalidNumber => "CLI_INVALID_NUMBER",
            Self::InvalidEncoding => "CLI_INVALID_ENCODING", Self::InvalidNeedle => "CLI_INVALID_NEEDLE",
            Self::IncompatibleOptions => "CLI_INCOMPATIBLE_OPTIONS",
        }
    }
}
impl std::fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for ArgumentError {}

pub fn json_requested(args: &[OsString]) -> bool {
    args.iter().take_while(|arg| *arg != "--").any(|arg| arg == "--json")
}

/// Excludes argv[0]. Numeric arguments are canonical unsigned decimal, not
/// floating-point, signed, whitespace-padded or silently saturating values.
pub fn parse(args: &[OsString]) -> Result<Arguments, ArgumentError> {
    if args.len() > MAX_ARGUMENTS { return Err(ArgumentError::Limit); }
    let mut total = 0usize;
    for arg in args {
        if arg.len() > MAX_SINGLE_ARGUMENT { return Err(ArgumentError::Limit); }
        total = total.checked_add(arg.len()).ok_or(ArgumentError::Limit)?;
        if total > MAX_ARGUMENT_BYTES { return Err(ArgumentError::Limit); }
    }
    let mut parsed = Arguments { command: Command::Launch, json: false, file: None,
        stdin: false, offset: 0, bytes: 64 * 1024, limit: 100, encoding: Encoding::Auto, needle: None };
    if args.is_empty() { return Ok(parsed); }
    let mut start = 1;
    parsed.command = match args[0].to_str() {
        Some("--help" | "-h" | "help") => Command::Help,
        Some("capabilities") => Command::Capabilities,
        Some("doctor") => Command::Doctor,
        Some("inspect") => Command::Inspect,
        Some("open") => Command::Open,
        Some("read") => Command::Read,
        Some("search") => Command::Search,
        Some("--json") => { start = 0; Command::Capabilities },
        Some("index" | "cache" | "trail" | "bench") => return Err(ArgumentError::UnknownCommand),
        _ => { start = 0; Command::Launch },
    };
    let mut seen = 0u16;
    let mut positional_only = false;
    let mut cursor = start;
    while cursor < args.len() {
        let arg = &args[cursor]; cursor += 1;
        if !positional_only && arg == "--" { positional_only = true; continue; }
        let option = if positional_only { None } else { arg.to_str().filter(|s| s.starts_with('-')) };
        if let Some(option) = option {
            let bit = match option {
                "--json" => 1, "--stdin" => 2, "--offset" => 4, "--bytes" => 8,
                "--limit" => 16, "--encoding" => 32, "--text" => 64, "--raw-hex" => 128,
                _ => return Err(ArgumentError::UnknownOption),
            };
            if seen & bit != 0 { return Err(ArgumentError::DuplicateOption); }
            seen |= bit;
            if option == "--json" { parsed.json = true; continue; }
            if option == "--stdin" { parsed.stdin = true; continue; }
            let value = args.get(cursor).ok_or(ArgumentError::MissingValue)?; cursor += 1;
            let value = value.to_str().ok_or(ArgumentError::MissingValue)?;
            match option {
                "--offset" => parsed.offset = decimal(value)?,
                "--bytes" => {
                    let value = decimal(value)?;
                    if !(4..=MAX_WINDOW_BYTES as u64).contains(&value) { return Err(ArgumentError::Limit); }
                    parsed.bytes = value as usize;
                }
                "--limit" => {
                    let value = decimal(value)?;
                    if value > MAX_RESULTS as u64 { return Err(ArgumentError::Limit); }
                    parsed.limit = value as usize;
                }
                "--encoding" => parsed.encoding = match value {
                    "auto" => Encoding::Auto, "utf8" => Encoding::Utf8,
                    "utf16le" => Encoding::Utf16Le, "utf16be" => Encoding::Utf16Be,
                    _ => return Err(ArgumentError::InvalidEncoding),
                },
                "--text" => {
                    if value.is_empty() || parsed.needle.is_some() { return Err(ArgumentError::InvalidNeedle); }
                    parsed.needle = Some(Needle::Text(value.to_owned()));
                }
                "--raw-hex" => {
                    if parsed.needle.is_some() { return Err(ArgumentError::InvalidNeedle); }
                    parsed.needle = Some(Needle::Raw(hex_bytes(value)?));
                }
                _ => return Err(ArgumentError::UnknownOption),
            }
        } else {
            if parsed.file.is_some() { return Err(ArgumentError::MultipleSources); }
            if arg.is_empty() { return Err(ArgumentError::MissingSource); }
            parsed.file = Some(PathBuf::from(arg));
        }
    }
    if parsed.stdin && parsed.file.is_some() { return Err(ArgumentError::MultipleSources); }
    match parsed.command {
        Command::Help | Command::Capabilities | Command::Doctor => {
            if parsed.file.is_some() || seen & !1 != 0 { return Err(ArgumentError::IncompatibleOptions); }
        }
        Command::Launch => {
            if seen != 0 { return Err(ArgumentError::IncompatibleOptions); }
        }
        Command::Inspect => {
            if parsed.file.is_none() { return Err(ArgumentError::MissingSource); }
            if seen & !1 != 0 { return Err(ArgumentError::IncompatibleOptions); }
        }
        Command::Open | Command::Read | Command::Search => {
            if parsed.file.is_none() && !parsed.stdin { return Err(ArgumentError::MissingSource); }
            if parsed.command == Command::Search {
                if parsed.needle.is_none() { return Err(ArgumentError::InvalidNeedle); }
            } else if seen & (16 | 64 | 128) != 0 { return Err(ArgumentError::IncompatibleOptions); }
            if parsed.stdin && parsed.offset != 0 { return Err(ArgumentError::IncompatibleOptions); }
            // A later live read of a header is not the same observation as the
            // selected range. Far-offset text therefore requires a declaration.
            if parsed.offset != 0 && parsed.encoding == Encoding::Auto
                && !matches!(parsed.needle, Some(Needle::Raw(_))) {
                return Err(ArgumentError::InvalidEncoding);
            }
            if matches!(parsed.needle, Some(Needle::Raw(_))) && seen & 32 != 0 {
                return Err(ArgumentError::IncompatibleOptions);
            }
        }
    }
    Ok(parsed)
}

pub fn decimal(text: &str) -> Result<u64, ArgumentError> {
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) { return Err(ArgumentError::InvalidNumber); }
    let mut result = 0u64;
    for byte in text.bytes() {
        if !byte.is_ascii_digit() { return Err(ArgumentError::InvalidNumber); }
        result = result.checked_mul(10).and_then(|n| n.checked_add(u64::from(byte - b'0')))
            .ok_or(ArgumentError::InvalidNumber)?;
    }
    Ok(result)
}
fn hex_bytes(text: &str) -> Result<Vec<u8>, ArgumentError> {
    if text.is_empty() || text.len() % 2 != 0 { return Err(ArgumentError::InvalidNeedle); }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(text.len() / 2).map_err(|_| ArgumentError::Limit)?;
    let digit = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'), b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10), _ => Err(ArgumentError::InvalidNeedle),
    };
    for pair in text.as_bytes().chunks_exact(2) { bytes.push((digit(pair[0])? << 4) | digit(pair[1])?); }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<OsString> { values.iter().map(OsString::from).collect() }
    #[test]
    fn no_machine_request_can_silently_open_the_gui() {
        assert_eq!(parse(&[]).unwrap().command, Command::Launch);
        assert_eq!(parse(&args(&["--json"])).unwrap().command, Command::Capabilities);
        assert_eq!(parse(&args(&["open", "file.rs", "--json"])).unwrap().command, Command::Open);
        assert!(parse(&args(&["index", "root", "--json"])).is_err());
    }
    #[test]
    fn delimiter_and_raw_paths_survive_parsing() {
        let parsed = parse(&args(&["read", "--json", "--", "--file with spaces.rs"])).unwrap();
        assert_eq!(parsed.file.unwrap(), PathBuf::from("--file with spaces.rs"));
        assert!(!json_requested(&args(&["read", "--", "--json"])));
        #[cfg(unix)] {
            use std::os::unix::ffi::OsStringExt;
            let native = OsString::from_vec(vec![b'x', 0xff]);
            let parsed = parse(&["read".into(), native.clone()]).unwrap();
            assert_eq!(parsed.file.unwrap().into_os_string(), native);
        }
    }
    #[test]
    fn full_width_decimal_does_not_round_or_wrap() {
        for value in [0, 1, (1u64 << 53) - 1, (1u64 << 53) + 1, u64::MAX] {
            assert_eq!(decimal(&value.to_string()), Ok(value));
        }
        for invalid in ["", "00", "01", "-1", "+1", "1e3", "1.0", " 1", "18446744073709551616"] {
            assert_eq!(decimal(invalid), Err(ArgumentError::InvalidNumber));
        }
    }
    #[test]
    fn duplicated_conflicting_and_excessive_options_fail_before_io() {
        for values in [vec!["read", "a", "--bytes", "4", "--bytes", "5"],
            vec!["search", "a", "--text", "x", "--raw-hex", "ff"],
            vec!["read", "a", "--stdin"], vec!["read", "a", "--bytes", "262145"],
            vec!["read", "a", "--offset", "100"], vec!["search", "a", "--text", ""],
            vec!["inspect", "a", "--offset", "0"], vec!["read", "a", "--encoding", "latin1"]] {
            assert!(parse(&args(&values)).is_err(), "{values:?}");
        }
        assert_eq!(parse(&vec![OsString::from("a"); 65]), Err(ArgumentError::Limit));
    }
    #[test]
    fn search_modes_are_explicit_and_hex_accepts_arbitrary_bytes() {
        let query = parse(&args(&["search", "--raw-hex", "00Ff", "--offset", "9007199254740993", "file"])).unwrap();
        assert_eq!(query.needle, Some(Needle::Raw(vec![0, 255])));
        assert_eq!(query.offset, (1u64 << 53) + 1);
        let text = parse(&args(&["search", "--stdin", "--text", "héllo world", "--encoding", "utf16le"])).unwrap();
        assert_eq!(text.needle, Some(Needle::Text("héllo world".to_owned())));
        for invalid in ["f", "gg", ""] { assert!(hex_bytes(invalid).is_err()); }
    }
}
