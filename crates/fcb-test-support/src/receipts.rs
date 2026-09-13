//! Shared bounded scenario-receipt infrastructure (fcb-wc0g).
//!
//! A scenario receipt is an evidence record for a test or probe run: identity
//! (scenario, seed, pin, route), a bounded redacted event ring, an optional
//! expected-vs-actual failure record, and a terminal exit/outcome record.
//! Receipt generation never declares qualification itself: a receipt is input
//! for an independent verifier, never a capability claim.
//!
//! Bounded by construction: every text field is truncated to a fixed budget,
//! the event ring saturates by dropping the oldest entries (the drop count is
//! preserved), and the encoded stream is capped with the terminal record kept
//! intact. The terminal record is the last line, prefixed with its payload
//! length, so truncation of the middle can never destroy the verdict; a
//! destroyed terminal record decodes to missing evidence, never to a
//! fabricated one.

use std::fmt;
use std::collections::VecDeque;

/// Fixed budgets. Chosen so a fully saturated receipt stays well under one
/// small page and can never grow with flood input.
pub const MAX_SCENARIO_LEN: usize = 64;
pub const MAX_ID_LEN: usize = 64;
pub const MAX_EVENT_BYTES: usize = 512;
pub const MAX_FAILURE_TEXT: usize = 512;
pub const MAX_RING_CAPACITY: usize = 4096;
pub const MAX_SENTINELS: usize = 64;
pub const MAX_SENTINEL_LEN: usize = 256;
pub const MAX_RECEIPT_BYTES: usize = 64 * 1024;
/// Upper bound reserved for the encoded terminal line when sizing the event
/// section. The real terminal line is always shorter than this.
const TERMINAL_LINE_RESERVE: usize = 8192;
/// Replacement marker written wherever a registered secret sentinel appears.
pub const REDACTION: &str = "[REDACTED]";
/// Suffix appended to text truncated at a budget boundary.
pub const TRUNCATION_SUFFIX: &str = "~";

/// Whether text was truncated, and the saturated original length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Truncated {
    pub truncated: bool,
    pub original_len: u64,
}

/// Text truncated to `max_bytes` on a UTF-8 character boundary with a
/// [`TRUNCATION_SUFFIX`] marker appended when truncation occurred.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedText {
    text: String,
    meta: Truncated,
}

impl BoundedText {
    pub fn new(text: &str, max_bytes: usize) -> Self {
        if text.len() <= max_bytes {
            return Self {
                text: text.to_string(),
                meta: Truncated {
                    truncated: false,
                    original_len: text.len() as u64,
                },
            };
        }
        let mut cut = max_bytes.saturating_sub(TRUNCATION_SUFFIX.len());
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut bounded = String::with_capacity(cut + TRUNCATION_SUFFIX.len());
        bounded.push_str(&text[..cut]);
        bounded.push_str(TRUNCATION_SUFFIX);
        Self {
            text: bounded,
            meta: Truncated {
                truncated: true,
                original_len: text.len() as u64,
            },
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub const fn meta(&self) -> Truncated {
        self.meta
    }
}

/// Bounded identifier: non-empty, at most [`MAX_ID_LEN`] bytes, and free of
/// control characters so it cannot forge extra lines in the encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedId(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdError {
    Empty,
    TooLong,
    ControlCharacter,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identifier is empty"),
            Self::TooLong => f.write_str("identifier exceeds the byte budget"),
            Self::ControlCharacter => f.write_str("identifier contains a control character"),
        }
    }
}

impl std::error::Error for IdError {}

impl BoundedId {
    pub fn new(text: &str) -> Result<Self, IdError> {
        if text.is_empty() {
            return Err(IdError::Empty);
        }
        if text.len() > MAX_ID_LEN {
            return Err(IdError::TooLong);
        }
        if text.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
            return Err(IdError::ControlCharacter);
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Registered secret sentinels. Every event, failure record, and identifier
/// passes through [`SecretSentinels::redact`] before it can enter a receipt;
/// occurrences of any registered sentinel are replaced with [`REDACTION`] and
/// counted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SecretSentinels {
    sentinels: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SentinelError {
    TooMany,
    SentinelTooLong,
}

impl fmt::Display for SentinelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany => f.write_str("too many secret sentinels"),
            Self::SentinelTooLong => f.write_str("secret sentinel exceeds the byte budget"),
        }
    }
}

impl std::error::Error for SentinelError {}

impl SecretSentinels {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, sentinel: &str) -> Result<(), SentinelError> {
        if sentinel.is_empty() || sentinel.len() > MAX_SENTINEL_LEN {
            return Err(SentinelError::SentinelTooLong);
        }
        if self.sentinels.len() >= MAX_SENTINELS {
            return Err(SentinelError::TooMany);
        }
        self.sentinels.push(sentinel.to_string());
        Ok(())
    }

    /// Replace every occurrence of every registered sentinel, longest first so
    /// overlapping sentinels redact maximally. Returns the redacted text and
    /// the number of replacements made.
    pub fn redact(&self, text: &str) -> (String, u64) {
        let mut ordered: Vec<&String> = self.sentinels.iter().collect();
        ordered.sort_by_key(|sentinel| std::cmp::Reverse(sentinel.len()));
        let mut out = text.to_string();
        let mut replacements = 0_u64;
        for sentinel in ordered {
            let mut start = 0;
            while let Some(found) = out[start..].find(sentinel.as_str()) {
                let at = start + found;
                out.replace_range(at..at + sentinel.len(), REDACTION);
                replacements += 1;
                start = at + REDACTION.len();
                if start > out.len() {
                    break;
                }
            }
        }
        (out, replacements)
    }
}

/// Class of a recorded event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    Note,
    ExpectedActual,
    Metric,
}

impl EventKind {
    const fn tag(self) -> &'static str {
        match self {
            Self::Note => "N",
            Self::ExpectedActual => "EA",
            Self::Metric => "M",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "N" => Some(Self::Note),
            "EA" => Some(Self::ExpectedActual),
            "M" => Some(Self::Metric),
            _ => None,
        }
    }
}

/// Saturation-bounded ring of redacted events. Pushing beyond capacity drops
/// the oldest event and counts the drop; the ring itself never grows.
#[derive(Clone, Debug)]
pub struct RedactedEventRing {
    capacity: usize,
    events: VecDeque<(EventKind, BoundedText)>,
    total_pushed: u64,
    dropped: u64,
    secrets_redacted: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RingError {
    CapacityOutOfBounds,
}

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityOutOfBounds => f.write_str("ring capacity outside the bounded range"),
        }
    }
}

impl std::error::Error for RingError {}

impl RedactedEventRing {
    pub fn new(capacity: usize) -> Result<Self, RingError> {
        if capacity == 0 || capacity > MAX_RING_CAPACITY {
            return Err(RingError::CapacityOutOfBounds);
        }
        Ok(Self {
            capacity,
            events: VecDeque::new(),
            total_pushed: 0,
            dropped: 0,
            secrets_redacted: 0,
        })
    }

    pub fn push(&mut self, sentinels: &SecretSentinels, kind: EventKind, text: &str) {
        let (redacted, replacements) = sentinels.redact(text);
        self.secrets_redacted += replacements;
        let bounded = BoundedText::new(&redacted, MAX_EVENT_BYTES);
        if self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back((kind, bounded));
        self.total_pushed += 1;
    }

    /// Drop the single oldest retained event, accounting it as dropped. Used
    /// by the encoder to fit the byte budget; the drop stays truthful.
    fn drop_oldest(&mut self) -> bool {
        if self.events.pop_front().is_some() {
            self.dropped += 1;
            true
        } else {
            false
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(EventKind, BoundedText)> {
        self.events.iter()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    pub const fn total_pushed(&self) -> u64 {
        self.total_pushed
    }

    pub const fn secrets_redacted(&self) -> u64 {
        self.secrets_redacted
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Expected-vs-actual record for a deliberate or observed failure. Both sides
/// are bounded and truncation-flagged independently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureRecord {
    pub expected: BoundedText,
    pub actual: BoundedText,
}

impl FailureRecord {
    pub fn new(expected: &str, actual: &str) -> Self {
        Self {
            expected: BoundedText::new(expected, MAX_FAILURE_TEXT),
            actual: BoundedText::new(actual, MAX_FAILURE_TEXT),
        }
    }
}

/// Terminal outcome of a scenario run. `MissingEvidence` marks a run whose
/// stream lost its terminal record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalOutcome {
    Success,
    Failure,
    Cancelled,
    MissingEvidence,
}

impl TerminalOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::MissingEvidence => "missing-evidence",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "success" => Some(Self::Success),
            "failure" => Some(Self::Failure),
            "cancelled" => Some(Self::Cancelled),
            "missing-evidence" => Some(Self::MissingEvidence),
            _ => None,
        }
    }
}

/// The terminal exit/outcome record. Encoded as the last line with its
/// payload length stated after the tag, so a decoder can always locate and
/// validate it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalRecord {
    pub outcome: TerminalOutcome,
    /// Process-style exit code, bounded to the conventional 0..=255 range.
    pub exit_code: u8,
    pub failure: Option<FailureRecord>,
    pub events_total: u64,
    pub events_dropped: u64,
    pub secrets_redacted: u64,
}

/// Identity of a scenario run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioIdentity {
    pub scenario: BoundedId,
    pub seed: u64,
    /// Optional source-capture pin this run was bound to.
    pub pin: Option<BoundedId>,
    /// Optional execution route label for the run.
    pub route: Option<BoundedId>,
}

/// A decoded receipt: the terminal record when one survived, plus how much of
/// the event history was recoverable. `terminal == None` means the stream is
/// missing (or lost) its terminal record: the run is classified
/// `MissingEvidence` and no verdict may be inferred from it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedReceipt {
    pub identity: ScenarioIdentity,
    pub terminal: Option<TerminalRecord>,
    pub events_recovered: Vec<(EventKind, String)>,
    /// Number of event lines present but unparseable after truncation.
    pub events_unreadable: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecError {
    NotAReceipt,
    TerminalCorrupt,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAReceipt => f.write_str("input is not a scenario receipt stream"),
            Self::TerminalCorrupt => f.write_str("terminal record is corrupt"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Escape a string onto a single canonical line: backslash, quote, and control
/// characters become fixed-width escapes; every other character passes
/// through. Tab is escaped so `\t` field separators stay unambiguous.
fn escape_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 || (character as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out
}

/// Inverse of [`escape_line`]. Returns `None` on malformed escapes.
fn unescape_line(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'x' => {
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() != 2 {
                    return None;
                }
                let value = u8::from_str_radix(&hex, 16).ok()?;
                if value < 0x20 || value == 0x7f {
                    out.push(value as char);
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(out)
}

fn escape_optional_id(id: &Option<BoundedId>) -> String {
    match id {
        Some(id) => escape_line(id.as_str()),
        None => "-".to_string(),
    }
}

fn parse_optional_id(text: &str) -> Result<Option<BoundedId>, CodecError> {
    if text == "-" {
        return Ok(None);
    }
    BoundedId::new(text)
        .map(Some)
        .map_err(|_| CodecError::TerminalCorrupt)
}

/// One scenario recorder: identity plus a redacted event ring. [`Self::finish`]
/// produces the terminal record and the bounded encoded stream in one step;
/// flood input shrinks the event section (oldest first, accounted in
/// `events_dropped`) and never the terminal record.
pub struct ScenarioRecorder {
    identity: ScenarioIdentity,
    sentinels: SecretSentinels,
    ring: RedactedEventRing,
    failure: Option<FailureRecord>,
}

impl ScenarioRecorder {
    pub fn new(
        identity: ScenarioIdentity,
        sentinels: SecretSentinels,
        ring_capacity: usize,
    ) -> Result<Self, RingError> {
        Ok(Self {
            identity,
            sentinels,
            ring: RedactedEventRing::new(ring_capacity)?,
            failure: None,
        })
    }

    pub fn event(&mut self, kind: EventKind, text: &str) {
        self.ring.push(&self.sentinels, kind, text);
    }

    /// Record an expected-vs-actual failure. Both sides are redacted before
    /// they are bounded and stored.
    pub fn failure(&mut self, expected: &str, actual: &str) {
        let (expected, _) = self.sentinels.redact(expected);
        let (actual, _) = self.sentinels.redact(actual);
        self.failure = Some(FailureRecord::new(&expected, &actual));
    }

    /// Produce the terminal record and the bounded encoded stream. A `Success`
    /// outcome clears any stale failure record: success carries no failure.
    pub fn finish(mut self, outcome: TerminalOutcome, exit_code: u8) -> (TerminalRecord, Vec<u8>) {
        if outcome == TerminalOutcome::Success {
            self.failure = None;
        }
        // Shrink the event section until the whole stream fits the budget,
        // dropping oldest first so the drop stays accounted in the ring (and
        // therefore in the terminal record).
        while self.encoded_size() > MAX_RECEIPT_BYTES {
            if !self.ring.drop_oldest() {
                break;
            }
        }
        let terminal = TerminalRecord {
            outcome,
            exit_code,
            failure: self.failure.take(),
            events_total: self.ring.total_pushed(),
            events_dropped: self.ring.dropped(),
            secrets_redacted: self.ring.secrets_redacted(),
        };
        let encoded = encode(&self.identity, &self.ring, &terminal);
        (terminal, encoded)
    }

    /// Upper-bound size of the encoded stream with the current ring contents,
    /// using [`TERMINAL_LINE_RESERVE`] for the terminal line.
    fn encoded_size(&self) -> usize {
        let header = header_line(&self.identity).len() + 1;
        let events: usize = self
            .ring
            .iter()
            .map(|(kind, text)| event_line(*kind, text.as_str()).len() + 1)
            .sum();
        header + events + TERMINAL_LINE_RESERVE
    }
}

fn header_line(identity: &ScenarioIdentity) -> String {
    format!(
        "FCBRECEIPT1 {} {} {} {}",
        escape_line(identity.scenario.as_str()),
        identity.seed,
        escape_optional_id(&identity.route),
        escape_optional_id(&identity.pin),
    )
}

fn event_line(kind: EventKind, text: &str) -> String {
    format!("E {} {}", kind.tag(), escape_line(text))
}

fn encode(
    identity: &ScenarioIdentity,
    ring: &RedactedEventRing,
    terminal: &TerminalRecord,
) -> Vec<u8> {
    let mut out = String::with_capacity(MAX_RECEIPT_BYTES.min(4096));
    out.push_str(&header_line(identity));
    out.push('\n');
    for (kind, text) in ring.iter() {
        out.push_str(&event_line(*kind, text.as_str()));
        out.push('\n');
    }
    let body = encode_terminal_body(terminal);
    out.push_str(&format!("T {} {}\n", body.len(), body));
    out.into_bytes()
}

fn encode_terminal_body(terminal: &TerminalRecord) -> String {
    let mut body = format!(
        "{} {} {} {} {}",
        terminal.outcome.as_str(),
        terminal.exit_code,
        terminal.events_total,
        terminal.events_dropped,
        terminal.secrets_redacted,
    );
    if let Some(failure) = &terminal.failure {
        body.push('\t');
        body.push_str(&format!(
            "F {}\t{}\t{}\t{}",
            escape_line(failure.expected.as_str()),
            escape_line(failure.actual.as_str()),
            failure.expected.meta().truncated as u8,
            failure.actual.meta().truncated as u8,
        ));
    }
    body
}

/// Decode a receipt stream. The terminal line is the last line beginning with
/// `T ` (event lines always begin with `E ` and the identity line with the
/// receipt tag); its payload length must match exactly or the verdict counts
/// as lost: the decoded receipt then has `terminal == None` (missing
/// evidence). Truncation inside the event section is reported through
/// `events_unreadable` and any counters the terminal record retained.
pub fn decode(bytes: &[u8]) -> Result<DecodedReceipt, CodecError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CodecError::NotAReceipt)?;
    if !text.starts_with("FCBRECEIPT1 ") {
        return Err(CodecError::NotAReceipt);
    }
    // The identity line is line 0; event lines are every line after it that is
    // not the terminal line. Event text cannot contain a literal newline (it
    // is escaped), so the first "\nT " begins the terminal line.
    let body_end = text.find("\nT ").map_or(text.len(), |at| at + 1);
    let mut lines = text[..body_end].lines();
    let header = lines.next().ok_or(CodecError::NotAReceipt)?;
    let identity = parse_header(header)?;
    let mut events_recovered = Vec::new();
    let mut events_unreadable = 0_u64;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        match parse_event_line(line) {
            Some(event) => events_recovered.push(event),
            None => events_unreadable += 1,
        }
    }
    let terminal = text
        .lines()
        .rev()
        .find(|line| line.starts_with("T "))
        .and_then(|line| parse_terminal_line(line).ok());
    Ok(DecodedReceipt {
        identity,
        terminal,
        events_recovered,
        events_unreadable,
    })
}

fn parse_header(header: &str) -> Result<ScenarioIdentity, CodecError> {
    let mut parts = header.splitn(5, ' ');
    let tag = parts.next().ok_or(CodecError::NotAReceipt)?;
    if tag != "FCBRECEIPT1" {
        return Err(CodecError::NotAReceipt);
    }
    let scenario = parts.next().ok_or(CodecError::NotAReceipt)?;
    let seed = parts.next().ok_or(CodecError::NotAReceipt)?;
    let route = parts.next().ok_or(CodecError::NotAReceipt)?;
    let pin = parts.next().unwrap_or("-");
    let scenario = unescape_line(scenario).ok_or(CodecError::NotAReceipt)?;
    let scenario = BoundedId::new(&scenario).map_err(|_| CodecError::TerminalCorrupt)?;
    let seed = seed.parse::<u64>().map_err(|_| CodecError::NotAReceipt)?;
    Ok(ScenarioIdentity {
        scenario,
        seed,
        pin: parse_optional_id(pin)?,
        route: parse_optional_id(route)?,
    })
}

fn parse_event_line(line: &str) -> Option<(EventKind, String)> {
    let rest = line.strip_prefix("E ")?;
    let (tag, text) = rest.split_once(' ')?;
    let kind = EventKind::from_tag(tag)?;
    Some((kind, unescape_line(text)?))
}

fn parse_terminal_line(line: &str) -> Result<TerminalRecord, CodecError> {
    let rest = line.strip_prefix("T ").ok_or(CodecError::TerminalCorrupt)?;
    let (length_text, body) = rest.split_once(' ').ok_or(CodecError::TerminalCorrupt)?;
    let length = length_text
        .parse::<usize>()
        .map_err(|_| CodecError::TerminalCorrupt)?;
    if body.len() != length {
        return Err(CodecError::TerminalCorrupt);
    }
    // Body layout: "<outcome> <exit> <total> <dropped> <redacted>" optionally
    // followed by "\tF <expected>\t<actual>\t<t>\t<a>". Escaped text cannot
    // contain literal tabs, so the first tab starts the failure section.
    let (counters, failure_text) = match body.split_once('\t') {
        Some((counters, failure)) => {
            let failure = failure
                .strip_prefix("F ")
                .ok_or(CodecError::TerminalCorrupt)?;
            (counters, Some(failure))
        }
        None => (body, None),
    };
    let mut counters = counters.split(' ');
    let outcome = counters.next().ok_or(CodecError::TerminalCorrupt)?;
    let outcome = TerminalOutcome::parse(outcome).ok_or(CodecError::TerminalCorrupt)?;
    let exit_code = counters
        .next()
        .and_then(|code| code.parse::<u8>().ok())
        .ok_or(CodecError::TerminalCorrupt)?;
    let events_total = counters
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(CodecError::TerminalCorrupt)?;
    let events_dropped = counters
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(CodecError::TerminalCorrupt)?;
    let secrets_redacted = counters
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(CodecError::TerminalCorrupt)?;
    let failure = match failure_text {
        Some(failure_text) => {
            let mut fields = failure_text.split('\t');
            let expected = fields.next().ok_or(CodecError::TerminalCorrupt)?;
            let actual = fields.next().ok_or(CodecError::TerminalCorrupt)?;
            let expected_truncated = fields
                .next()
                .and_then(|flag| flag.parse::<u8>().ok())
                .ok_or(CodecError::TerminalCorrupt)?;
            let actual_truncated = fields
                .next()
                .and_then(|flag| flag.parse::<u8>().ok())
                .ok_or(CodecError::TerminalCorrupt)?;
            if fields.next().is_some() {
                return Err(CodecError::TerminalCorrupt);
            }
            let expected = unescape_line(expected).ok_or(CodecError::TerminalCorrupt)?;
            let actual = unescape_line(actual).ok_or(CodecError::TerminalCorrupt)?;
            let expected_len = expected.len() as u64;
            let actual_len = actual.len() as u64;
            if expected_truncated > 1 || actual_truncated > 1 {
                return Err(CodecError::TerminalCorrupt);
            }
            Some(FailureRecord {
                expected: BoundedText {
                    text: expected,
                    meta: Truncated {
                        truncated: expected_truncated == 1,
                        original_len: expected_len,
                    },
                },
                actual: BoundedText {
                    text: actual,
                    meta: Truncated {
                        truncated: actual_truncated == 1,
                        original_len: actual_len,
                    },
                },
            })
        }
        None => None,
    };
    Ok(TerminalRecord {
        outcome,
        exit_code,
        failure,
        events_total,
        events_dropped,
        secrets_redacted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ScenarioIdentity {
        ScenarioIdentity {
            scenario: BoundedId::new("scenario-receipts").unwrap(),
            seed: 0x1234_5678_9abc_def0,
            pin: Some(BoundedId::new("pin-7").unwrap()),
            route: Some(BoundedId::new("route-headless").unwrap()),
        }
    }

    fn sentinels() -> SecretSentinels {
        let mut sentinels = SecretSentinels::new();
        sentinels.register("super-secret-token").unwrap();
        sentinels
    }

    fn recorder() -> ScenarioRecorder {
        ScenarioRecorder::new(identity(), sentinels(), 8).unwrap()
    }

    #[test]
    fn round_trip_keeps_identity_events_failure_and_terminal() {
        let mut recorder = recorder();
        recorder.event(EventKind::Note, "started with 3 files");
        recorder.event(EventKind::Metric, "bytes=512");
        recorder.event(EventKind::ExpectedActual, "line differs at 42");
        let (terminal, encoded) = recorder.finish(TerminalOutcome::Failure, 1);
        assert_eq!(terminal.outcome, TerminalOutcome::Failure);
        assert_eq!(terminal.exit_code, 1);
        assert!(terminal.failure.is_some());

        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.identity.seed, identity().seed);
        assert_eq!(decoded.identity.scenario, identity().scenario);
        assert_eq!(decoded.identity.pin, identity().pin);
        assert_eq!(decoded.identity.route, identity().route);
        let decoded_terminal = decoded.terminal.unwrap();
        assert_eq!(decoded_terminal.outcome, TerminalOutcome::Failure);
        assert_eq!(decoded_terminal.exit_code, 1);
        assert_eq!(decoded_terminal.events_total, 3);
        assert_eq!(decoded.events_recovered.len(), 3);
        assert_eq!(decoded.events_unreadable, 0);
    }

    #[test]
    fn ring_saturation_drops_oldest_and_counts() {
        let mut recorder = ScenarioRecorder::new(identity(), sentinels(), 4).unwrap();
        for index in 0..10 {
            recorder.event(EventKind::Note, &format!("event-{index}"));
        }
        let (terminal, encoded) = recorder.finish(TerminalOutcome::Success, 0);
        assert_eq!(terminal.events_total, 10);
        assert_eq!(terminal.events_dropped, 6);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.events_recovered.len(), 4);
        assert_eq!(decoded.events_recovered[0].1, "event-6");
        assert_eq!(decoded.events_recovered[3].1, "event-9");
    }

    #[test]
    fn secret_sentinels_never_reach_the_encoded_stream() {
        let mut recorder = recorder();
        recorder.event(EventKind::Note, "auth=super-secret-token ok");
        recorder.failure("expected super-secret-token", "got super-secret-token instead");
        let (terminal, encoded) = recorder.finish(TerminalOutcome::Failure, 2);
        assert!(terminal.secrets_redacted >= 3);
        let encoded_text = std::str::from_utf8(&encoded).unwrap();
        assert!(!encoded_text.contains("super-secret-token"));
        assert!(encoded_text.contains(REDACTION));
        let decoded = decode(&encoded).unwrap();
        let decoded_terminal = decoded.terminal.unwrap();
        let failure = decoded_terminal.failure.unwrap();
        assert!(failure.expected.as_str().contains(REDACTION));
        assert!(!failure.actual.as_str().contains("super-secret-token"));
    }

    #[test]
    fn flood_stays_bounded_and_terminal_survives() {
        let mut recorder = ScenarioRecorder::new(identity(), sentinels(), 64).unwrap();
        for index in 0..20_000 {
            recorder.event(
                EventKind::Note,
                &format!("flood-{index}-payload-0123456789abcdef"),
            );
        }
        let (terminal, encoded) = recorder.finish(TerminalOutcome::Failure, 3);
        assert!(encoded.len() <= MAX_RECEIPT_BYTES);
        assert_eq!(terminal.outcome, TerminalOutcome::Failure);
        assert_eq!(terminal.events_total, 20_000);
        // Ring capacity 64 retained 64; the encoder shrank the rest, and every
        // dropped event is accounted.
        assert_eq!(
            terminal.events_total - terminal.events_dropped,
            u64::try_from(terminal_len_events(&encoded)).unwrap_or(u64::MAX)
        );
        let decoded = decode(&encoded).unwrap();
        let decoded_terminal = decoded.terminal.unwrap();
        assert_eq!(decoded_terminal.outcome, TerminalOutcome::Failure);
        assert_eq!(decoded_terminal.exit_code, 3);
        assert_eq!(decoded_terminal.events_total, 20_000);
        assert!(decoded_terminal.events_dropped > 0);
        assert!(decoded.events_unreadable == 0);
    }

    /// Number of events the encoded stream actually retained.
    fn terminal_len_events(encoded: &[u8]) -> usize {
        decode(encoded).unwrap().events_recovered.len()
    }

    #[test]
    fn middle_truncation_preserves_the_verdict_and_reports_loss() {
        let mut recorder = ScenarioRecorder::new(identity(), sentinels(), 32).unwrap();
        for index in 0..32 {
            recorder.event(EventKind::Note, &format!("keep-{index}-0123456789abcdef"));
        }
        let (_terminal, encoded) = recorder.finish(TerminalOutcome::Failure, 1);
        let mut truncated = encoded.clone();
        let start = truncated.len() / 3;
        let end = 2 * (truncated.len() / 3);
        truncated.drain(start..end);
        let decoded = decode(&truncated).unwrap();
        let decoded_terminal = decoded.terminal.unwrap();
        assert_eq!(decoded_terminal.outcome, TerminalOutcome::Failure);
        assert_eq!(decoded_terminal.exit_code, 1);
        assert!(decoded.events_recovered.len() < 32);
        assert!(decoded.events_unreadable > 0);
    }

    #[test]
    fn cancelled_runs_carry_no_failure_record() {
        let mut recorder = recorder();
        recorder.event(EventKind::Note, "cancel requested");
        let (terminal, encoded) = recorder.finish(TerminalOutcome::Cancelled, 130);
        assert!(terminal.failure.is_none());
        let decoded = decode(&encoded).unwrap();
        let decoded_terminal = decoded.terminal.unwrap();
        assert_eq!(decoded_terminal.outcome, TerminalOutcome::Cancelled);
        assert_eq!(decoded_terminal.exit_code, 130);
        assert!(decoded_terminal.failure.is_none());
    }

    #[test]
    fn destroyed_terminal_decodes_to_missing_evidence_inputs() {
        let mut recorder = recorder();
        recorder.event(EventKind::Note, "half a run");
        let (_terminal, encoded) = recorder.finish(TerminalOutcome::Success, 0);
        let cut = encoded.len() - 8;
        let decoded = decode(&encoded[..cut]).unwrap();
        assert!(decoded.terminal.is_none());
        assert!(!decoded.events_recovered.is_empty());
    }

    #[test]
    fn non_receipt_input_is_rejected() {
        assert_eq!(decode(b"hello world"), Err(CodecError::NotAReceipt));
        assert_eq!(decode(b""), Err(CodecError::NotAReceipt));
    }

    #[test]
    fn corrupt_terminal_length_is_not_parsed_as_a_verdict() {
        let mut recorder = recorder();
        recorder.event(EventKind::Note, "note");
        let (_terminal, encoded) = recorder.finish(TerminalOutcome::Success, 0);
        let mut corrupt = encoded.clone();
        let terminal_at = corrupt
            .windows(2)
            .rposition(|window| window == b"\nT")
            .unwrap();
        // The stated payload length begins two bytes after the tag: corrupt it.
        let length_at = terminal_at + 3;
        assert!(corrupt[length_at].is_ascii_digit());
        corrupt[length_at] = if corrupt[length_at] == b'9' {
            b'8'
        } else {
            b'9'
        };
        let decoded = decode(&corrupt).unwrap();
        // The corrupt line cannot validate as a terminal record; the decoder
        // must not fabricate a verdict from it.
        assert!(decoded.terminal.is_none());
        assert!(!decoded.events_recovered.is_empty());
    }

    #[test]
    fn identifiers_reject_control_characters_and_emptiness() {
        assert_eq!(BoundedId::new(""), Err(IdError::Empty));
        assert_eq!(BoundedId::new("bad\nid"), Err(IdError::ControlCharacter));
        let long = "x".repeat(MAX_ID_LEN + 1);
        assert_eq!(BoundedId::new(&long), Err(IdError::TooLong));
        let okay = BoundedId::new("route-headless").unwrap();
        assert_eq!(okay.as_str(), "route-headless");
    }

    #[test]
    fn ring_capacity_bounds_are_enforced() {
        assert_eq!(
            RedactedEventRing::new(0).unwrap_err(),
            RingError::CapacityOutOfBounds
        );
        assert_eq!(
            RedactedEventRing::new(MAX_RING_CAPACITY + 1).unwrap_err(),
            RingError::CapacityOutOfBounds
        );
        assert!(RedactedEventRing::new(1).is_ok());
    }

    #[test]
    fn bounded_text_truncates_on_character_boundaries() {
        let emoji = "a\u{1f600}\u{1f600}\u{1f600}";
        let bounded = BoundedText::new(emoji, 6);
        assert!(bounded.meta().truncated);
        assert!(bounded.as_str().ends_with(TRUNCATION_SUFFIX));
        assert_eq!(bounded.meta().original_len, emoji.len() as u64);
        let fitting = BoundedText::new("small", 32);
        assert!(!fitting.meta().truncated);
    }

    #[test]
    fn success_finish_clears_stale_failure_records() {
        let mut recorder = recorder();
        recorder.failure("expected", "actual");
        let (terminal, encoded) = recorder.finish(TerminalOutcome::Success, 0);
        assert!(terminal.failure.is_none());
        let decoded = decode(&encoded).unwrap();
        assert!(decoded.terminal.unwrap().failure.is_none());
    }

    #[test]
    fn multiline_event_text_cannot_forge_extra_lines() {
        let mut recorder = recorder();
        recorder.event(EventKind::Note, "line one\nE N forged line");
        let (_terminal, encoded) = recorder.finish(TerminalOutcome::Success, 0);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.events_recovered.len(), 1);
        assert_eq!(
            decoded.events_recovered[0].1,
            "line one\nE N forged line"
        );
    }
}
