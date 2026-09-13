//! Shared bounded scenario-receipt infrastructure (fcb-wc0g).
//!
//! A scenario receipt is an evidence record for one test or probe run:
//! identity (scenario, seed, source pin, route), the corpus identity it ran
//! against, a saturation-bounded redacted event ring, an optional
//! expected-vs-actual comparison, artifact references, and the terminal
//! outcome (effect, optional exit code, optional unexecuted reason).
//!
//! Receipt generation never declares qualification itself: a receipt records
//! what happened and carries the evidence; mapping a receipt to a pass/fail
//! verdict belongs to an independent verifier.
//!
//! Bounded by construction: every string is redacted through the scenario
//! [`Redactor`] before storage, [`BoundedText`] keeps the newest
//! [`MAX_FIELD_BYTES`] bytes on a character boundary (truncation flags and
//! original sizes are preserved truthfully), the event ring drops its oldest
//! events at capacity while keeping exact counters, and
//! [`ScenarioReceipt::from_draft`] retains the newest [`RECEIPT_MAX_EVENTS`]
//! events. The codec is line-oriented and canonical: `decode(encode(x)) ==
//! x`, re-encoding is byte-identical, and a foreign or truncated stream
//! yields a typed error instead of an invented verdict.

use std::collections::VecDeque;
use std::fmt;

use crate::ContentDigest;

/// Schema tag carried as the first line of every encoded receipt.
pub const RECEIPT_SCHEMA: &str = "fcb.receipt.v1";
/// Marker written wherever a registered sentinel appears.
pub const REDACTED_TOKEN: &str = "[REDACTED]";
/// Byte budget for any single stored text field.
pub const MAX_FIELD_BYTES: usize = 1024;
/// Longest sentinel the redactor accepts.
pub const MAX_SENTINEL_BYTES: usize = 256;
/// Events retained per receipt; older events are summarized, not stored.
pub const RECEIPT_MAX_EVENTS: usize = 256;
/// Default live-ring capacity.
pub const DEFAULT_RING_CAPACITY: usize = 64;
/// Upper bound on identifiers (source pins are exactly 40 bytes).
pub const MAX_ID_BYTES: usize = 128;

/// A commit-shaped source identity: exactly 40 lowercase hex bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePin(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinError {
    WrongLength,
    NotHex,
}

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength => f.write_str("source pin must be exactly 40 bytes"),
            Self::NotHex => f.write_str("source pin must be lowercase hex"),
        }
    }
}

impl std::error::Error for PinError {}

impl SourcePin {
    pub fn new(text: &str) -> Result<Self, PinError> {
        if text.len() != 40 {
            return Err(PinError::WrongLength);
        }
        if !text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(PinError::NotHex);
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A bounded execution-route label: non-empty, at most
/// [`MAX_ID_BYTES`], free of spaces and control characters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteId(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteError {
    Empty,
    TooLong,
    InvalidCharacter,
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("route identifier is empty"),
            Self::TooLong => f.write_str("route identifier exceeds the byte budget"),
            Self::InvalidCharacter => f.write_str("route identifier contains a forbidden byte"),
        }
    }
}

impl std::error::Error for RouteError {}

impl RouteId {
    pub fn new(text: &str) -> Result<Self, RouteError> {
        if text.is_empty() {
            return Err(RouteError::Empty);
        }
        if text.len() > MAX_ID_BYTES {
            return Err(RouteError::TooLong);
        }
        if text.bytes().any(|byte| byte == b' ' || byte < 0x20 || byte == 0x7f) {
            return Err(RouteError::InvalidCharacter);
        }
        Ok(Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Deterministic scenario seed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ScenarioSeed(pub u64);

/// Secret-sentinel redactor. Registered sentinels are replaced with
/// [`REDACTED_TOKEN`] wherever text enters a receipt; an empty sentinel is
/// accepted but ignored (it cannot match anything safely).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Redactor {
    sentinels: Vec<String>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style registration. Empty sentinels are ignored.
    pub fn with_sentinel(mut self, sentinel: &str) -> Self {
        if !sentinel.is_empty() && sentinel.len() <= MAX_SENTINEL_BYTES {
            self.sentinels.push(sentinel.to_string());
        }
        self
    }

    /// Replace every registered-sentinel occurrence, longest first. Returns
    /// the redacted text.
    pub fn redact(&self, text: &str) -> String {
        let mut ordered: Vec<&String> = self.sentinels.iter().collect();
        ordered.sort_by_key(|sentinel| std::cmp::Reverse(sentinel.len()));
        let mut out = text.to_string();
        for sentinel in ordered {
            let mut start = 0;
            while let Some(found) = out[start..].find(sentinel.as_str()) {
                let at = start + found;
                out.replace_range(at..at + sentinel.len(), REDACTED_TOKEN);
                start = at + REDACTED_TOKEN.len();
                if start > out.len() {
                    break;
                }
            }
        }
        out
    }
}

/// Text already through the redactor, bounded to [`MAX_FIELD_BYTES`] bytes.
///
/// Truncation keeps the newest bytes (the tail) on a UTF-8 character
/// boundary: recent evidence survives, the truncation flag is set, and the
/// original byte count is preserved exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedText {
    text: String,
    truncated: bool,
    original_bytes: u64,
}

impl BoundedText {
    /// Bound text that has already been redacted.
    pub fn from_redacted(text: &str) -> Self {
        let original = text.len() as u64;
        if text.len() <= MAX_FIELD_BYTES {
            return Self {
                text: text.to_string(),
                truncated: false,
                original_bytes: original,
            };
        }
        let mut start = text.len() - MAX_FIELD_BYTES;
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        Self {
            text: text[start..].to_string(),
            truncated: true,
            original_bytes: original,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    pub const fn original_bytes(&self) -> u64 {
        self.original_bytes
    }
}

/// One retained ring event: its ring-assigned sequence, the bounded message,
/// and truthful truncation metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    sequence: u64,
    message: BoundedText,
}

impl Event {
    pub fn message(&self) -> &str {
        self.message.text()
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn truncated(&self) -> bool {
        self.message.truncated()
    }

    pub const fn original_bytes(&self) -> u64 {
        self.message.original_bytes()
    }
}

/// Saturation-bounded ring of redacted events. At capacity, the oldest event
/// is dropped and [`Self::dropped`] counts it; sequence numbers keep the
/// full-history position of every retained event.
#[derive(Clone, Debug)]
pub struct EventRing {
    capacity: usize,
    next_sequence: u64,
    dropped: u64,
    events: VecDeque<Event>,
}

impl EventRing {
    /// Any capacity is accepted; saturation is handled by dropping.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            next_sequence: 0,
            dropped: 0,
            events: VecDeque::new(),
        }
    }

    pub fn push(&mut self, redactor: &Redactor, message: &str) {
        let redacted = redactor.redact(message);
        let bounded = BoundedText::from_redacted(&redacted);
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        if self.capacity > 0 && self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped += 1;
        }
        if self.capacity > 0 {
            self.events.push_back(Event {
                sequence,
                message: bounded,
            });
        }
    }

    pub fn events(&self) -> impl Iterator<Item = &Event> {
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

    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Recorded expected-vs-actual comparison. Both sides are redacted at
/// construction and bounded independently.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedVsActual {
    expected: BoundedText,
    actual: BoundedText,
}

impl ExpectedVsActual {
    pub fn new(redactor: &Redactor, expected: &str, actual: &str) -> Self {
        Self {
            expected: BoundedText::from_redacted(&redactor.redact(expected)),
            actual: BoundedText::from_redacted(&redactor.redact(actual)),
        }
    }

    pub const fn expected(&self) -> &BoundedText {
        &self.expected
    }

    pub const fn actual(&self) -> &BoundedText {
        &self.actual
    }
}

/// What actually happened to the run: the effect, the process exit code when
/// the run reached one, and, for runs that never executed to a terminal
/// state, the reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalOutcome {
    effect: Effect,
    exit_code: Option<i32>,
    unexecuted_reason: Option<String>,
}

/// Terminal effect vocabulary. This is the only verdict carrier in the
/// receipt API; nothing else maps a receipt to pass or fail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    Succeeded,
    Failed,
    Canceled,
}

impl Effect {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

impl TerminalOutcome {
    pub const fn new(
        exit_code: Option<i32>,
        effect: Effect,
        unexecuted_reason: Option<String>,
    ) -> Self {
        Self {
            effect,
            exit_code,
            unexecuted_reason,
        }
    }

    pub const fn effect(&self) -> Effect {
        self.effect
    }

    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn unexecuted_reason(&self) -> Option<&str> {
        self.unexecuted_reason.as_deref()
    }
}

/// Where the receipt's event section stands relative to the live ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RingSummary {
    /// Live ring capacity the events came from.
    pub capacity: usize,
    /// Events the receipt retains.
    pub kept: usize,
    /// Events lost to ring saturation plus receipt-level retention.
    pub dropped: u64,
    /// Sequence the live ring would assign to its next push.
    pub next_sequence: u64,
    /// True when the receipt retained fewer events than the ring held.
    pub receipt_truncated: bool,
}

/// Pre-construction description of a scenario run. Strings in `scenario` and
/// `artifacts` are redacted by [`ScenarioReceipt::from_draft`]; ring events
/// are redacted at [`EventRing::push`].
#[derive(Clone, Debug)]
pub struct ScenarioReceiptDraft {
    pub scenario: String,
    pub seed: ScenarioSeed,
    pub pin: SourcePin,
    pub route: RouteId,
    pub corpus_digest: ContentDigest,
    pub corpus_count: u64,
    pub outcome: TerminalOutcome,
    pub comparison: Option<ExpectedVsActual>,
    pub ring: EventRing,
    pub artifacts: Vec<String>,
}

/// The immutable evidence record for one scenario run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioReceipt {
    schema: &'static str,
    scenario: String,
    seed: ScenarioSeed,
    pin: SourcePin,
    route: RouteId,
    corpus_digest: ContentDigest,
    corpus_count: u64,
    outcome: TerminalOutcome,
    comparison: Option<ExpectedVsActual>,
    ring_summary: RingSummary,
    events: Vec<Event>,
    artifacts: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptError {
    /// The stream's schema tag is foreign or missing.
    SchemaMismatch,
    /// A row is truncated, malformed, or required-but-absent.
    MalformedRow,
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaMismatch => f.write_str("receipt schema does not match"),
            Self::MalformedRow => f.write_str("receipt row is malformed or missing"),
        }
    }
}

impl std::error::Error for ReceiptError {}

/// Escape a string onto one canonical line: backslash, control characters,
/// and newline/carriage-return/tab become fixed-width escapes.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
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

/// Inverse of [`escape_text`]; `None` on malformed escapes.
fn unescape_text(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'x' => {
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() != 2 {
                    return None;
                }
                let value = u8::from_str_radix(&hex, 16).ok()?;
                if (0x20..0x7f).contains(&value) || value >= 0x80 {
                    return None;
                }
                out.push(value as char);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Length-prefixed field value: `name:<len>:<escaped>`.
fn length_field(name: &str, text: &str) -> String {
    format!("{name}:{}:{text}\n", text.len())
}

/// Parse `name:<len>:<escaped>` after the `name:` prefix.
fn parse_length_field(payload: &str) -> Option<String> {
    let (length, value) = payload.split_once(':')?;
    let length = length.parse::<usize>().ok()?;
    if value.len() != length {
        return None;
    }
    unescape_text(value)
}

impl ScenarioReceipt {
    /// Build a receipt from a draft. The scenario name and every artifact
    /// reference pass through the redactor; the ring contributes its newest
    /// [`RECEIPT_MAX_EVENTS`] events, with any excess accounted as dropped.
    pub fn from_draft(redactor: &Redactor, draft: ScenarioReceiptDraft) -> Self {
        let scenario = redactor.redact(&draft.scenario);
        let artifacts: Vec<String> = draft
            .artifacts
            .iter()
            .map(|artifact| redactor.redact(artifact))
            .collect();

        // Retain the newest RECEIPT_MAX_EVENTS events, oldest-first, and
        // account every dropped event truthfully.
        let total = draft.ring.len() as u64;
        let kept = total.min(RECEIPT_MAX_EVENTS as u64);
        let skip = (total - kept) as usize;
        let events: Vec<Event> = draft.ring.events().skip(skip).cloned().collect();
        let receipt_truncated = skip > 0;
        let ring_summary = RingSummary {
            capacity: draft.ring.capacity(),
            kept: events.len(),
            dropped: draft.ring.dropped() + skip as u64,
            next_sequence: draft.ring.next_sequence(),
            receipt_truncated,
        };

        Self {
            schema: RECEIPT_SCHEMA,
            scenario,
            seed: draft.seed,
            pin: draft.pin,
            route: draft.route,
            corpus_digest: draft.corpus_digest,
            corpus_count: draft.corpus_count,
            outcome: draft.outcome,
            comparison: draft.comparison,
            ring_summary,
            events,
            artifacts,
        }
    }

    /// Encode into the canonical line format. Row order is fixed; strings are
    /// length-prefixed and escaped so no payload can forge extra rows.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::with_capacity(1024);
        out.push_str(self.schema);
        out.push('\n');
        out.push_str(&length_field("scenario", &self.scenario));
        out.push_str(&format!("seed:{}\n", self.seed.0));
        out.push_str(&format!("pin:{}\n", self.pin.as_str()));
        out.push_str(&length_field("route", self.route.as_str()));
        out.push_str(&format!("corpus_digest:{}\n", self.corpus_digest.hex()));
        out.push_str(&format!("corpus_count:{}\n", self.corpus_count));
        out.push_str(&format!("effect:{}\n", self.outcome.effect.as_str()));
        match self.outcome.exit_code {
            Some(code) => out.push_str(&format!("exit:{code}\n")),
            None => out.push_str("exit:-\n"),
        }
        match &self.outcome.unexecuted_reason {
            Some(reason) => out.push_str(&length_field("reason", reason)),
            None => out.push_str("reason:-\n"),
        }
        out.push_str(&format!(
            "ring:{}:{}:{}:{}:{}\n",
            self.ring_summary.capacity,
            self.ring_summary.kept,
            self.ring_summary.dropped,
            self.ring_summary.next_sequence,
            self.ring_summary.receipt_truncated as u8,
        ));
        match &self.comparison {
            Some(comparison) => {
                out.push_str(&length_field(
                    "comparison_expected",
                    comparison.expected.text(),
                ));
                out.push_str(&format!(
                    "comparison_expected_truncated:{}\n",
                    comparison.expected.truncated() as u8
                ));
                out.push_str(&format!(
                    "comparison_expected_original:{}\n",
                    comparison.expected.original_bytes()
                ));
                out.push_str(&length_field("comparison_actual", comparison.actual.text()));
                out.push_str(&format!(
                    "comparison_actual_truncated:{}\n",
                    comparison.actual.truncated() as u8
                ));
                out.push_str(&format!(
                    "comparison_actual_original:{}\n",
                    comparison.actual.original_bytes()
                ));
            }
            None => out.push_str("comparison:-\n"),
        }
        out.push_str(&format!("events:{}\n", self.events.len()));
        for event in &self.events {
            out.push_str(&format!(
                "event:{}:{}:{}:{}:{}\n",
                event.sequence,
                event.message.truncated() as u8,
                event.message.original_bytes(),
                event.message.text().len(),
                escape_text(event.message.text()),
            ));
        }
        out.push_str(&format!("artifacts:{}\n", self.artifacts.len()));
        for artifact in &self.artifacts {
            out.push_str(&length_field("artifact", artifact));
        }
        out.into_bytes()
    }

    /// Decode a canonical receipt stream. Foreign schema tags are rejected as
    /// [`ReceiptError::SchemaMismatch`]; truncated or malformed rows as
    /// [`ReceiptError::MalformedRow`]. Decoding never invents evidence.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReceiptError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ReceiptError::MalformedRow)?;
        let mut lines = text.lines();
        let schema = lines.next().ok_or(ReceiptError::MalformedRow)?;
        if schema != RECEIPT_SCHEMA {
            return Err(ReceiptError::SchemaMismatch);
        }

        let mut rows = lines.map(str::to_string).collect::<VecDeque<String>>();
        let mut next_name = |expected: &str| -> Result<String, ReceiptError> {
            let row = rows.pop_front().ok_or(ReceiptError::MalformedRow)?;
            let (name, payload) = row.split_once(':').ok_or(ReceiptError::MalformedRow)?;
            if name != expected {
                return Err(ReceiptError::MalformedRow);
            }
            Ok(payload.to_string())
        };

        let scenario = parse_length_field(&next_name("scenario")?)
            .ok_or(ReceiptError::MalformedRow)?;
        let seed = next_name("seed")?
            .parse::<u64>()
            .map_err(|_| ReceiptError::MalformedRow)?;
        let pin_text = next_name("pin")?;
        let pin = SourcePin::new(&pin_text).map_err(|_| ReceiptError::MalformedRow)?;
        let route_text = next_name("route")?;
        let route = RouteId::new(&parse_length_field(&route_text).ok_or(ReceiptError::MalformedRow)?)
            .map_err(|_| ReceiptError::MalformedRow)?;
        let digest_text = next_name("corpus_digest")?;
        let corpus_digest = ContentDigest::from_hex(&digest_text)
            .ok_or(ReceiptError::MalformedRow)?;
        let corpus_count = next_name("corpus_count")?
            .parse::<u64>()
            .map_err(|_| ReceiptError::MalformedRow)?;
        let effect = Effect::parse(&next_name("effect")?).ok_or(ReceiptError::MalformedRow)?;
        let exit_text = next_name("exit")?;
        let exit_code = if exit_text == "-" {
            None
        } else {
            Some(exit_text.parse::<i32>().map_err(|_| ReceiptError::MalformedRow)?)
        };
        let reason_text = next_name("reason")?;
        let unexecuted_reason = if reason_text == "-" {
            None
        } else {
            Some(parse_length_field(&reason_text).ok_or(ReceiptError::MalformedRow)?)
        };
        let ring_row = next_name("ring")?;
        let mut ring_parts = ring_row.split(':');
        let mut next_number = || -> Result<u64, ReceiptError> {
            ring_parts
                .next()
                .ok_or(ReceiptError::MalformedRow)?
                .parse::<u64>()
                .map_err(|_| ReceiptError::MalformedRow)
        };
        let capacity = next_number()? as usize;
        let kept = next_number()? as usize;
        let dropped = next_number()?;
        let next_sequence = next_number()?;
        let receipt_truncated = next_number()? == 1;
        if next_number().is_ok() {
            return Err(ReceiptError::MalformedRow);
        }
        let ring_summary = RingSummary {
            capacity,
            kept,
            dropped,
            next_sequence,
            receipt_truncated,
        };

        let comparison = {
            let marker = next_name("comparison")?;
            if marker == "-" {
                None
            } else if marker == "present" {
                let expected = parse_length_field(&next_name("comparison_expected")?)
                    .ok_or(ReceiptError::MalformedRow)?;
                let expected_truncated = next_name("comparison_expected_truncated")?
                    .parse::<u8>()
                    .map_err(|_| ReceiptError::MalformedRow)?;
                let expected_original = next_name("comparison_expected_original")?
                    .parse::<u64>()
                    .map_err(|_| ReceiptError::MalformedRow)?;
                let actual = parse_length_field(&next_name("comparison_actual")?)
                    .ok_or(ReceiptError::MalformedRow)?;
                let actual_truncated = next_name("comparison_actual_truncated")?
                    .parse::<u8>()
                    .map_err(|_| ReceiptError::MalformedRow)?;
                let actual_original = next_name("comparison_actual_original")?
                    .parse::<u64>()
                    .map_err(|_| ReceiptError::MalformedRow)?;
                if expected_truncated > 1 || actual_truncated > 1 {
                    return Err(ReceiptError::MalformedRow);
                }
                Some(ExpectedVsActual {
                    expected: BoundedText {
                        text: expected,
                        truncated: expected_truncated == 1,
                        original_bytes: expected_original,
                    },
                    actual: BoundedText {
                        text: actual,
                        truncated: actual_truncated == 1,
                        original_bytes: actual_original,
                    },
                })
            } else {
                return Err(ReceiptError::MalformedRow);
            }
        };

        let event_count = next_name("events")?
            .parse::<usize>()
            .map_err(|_| ReceiptError::MalformedRow)?;
        if event_count > RECEIPT_MAX_EVENTS {
            return Err(ReceiptError::MalformedRow);
        }
        let mut events = Vec::with_capacity(event_count);
        for _ in 0..event_count {
            let row = next_name("event")?;
            let mut fields = row.split(':');
            let sequence = fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(ReceiptError::MalformedRow)?;
            let truncated = fields
                .next()
                .and_then(|value| value.parse::<u8>().ok())
                .ok_or(ReceiptError::MalformedRow)?;
            let original_bytes = fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(ReceiptError::MalformedRow)?;
            let length = fields
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or(ReceiptError::MalformedRow)?;
            let message = fields.next().ok_or(ReceiptError::MalformedRow)?;
            if fields.next().is_some() {
                return Err(ReceiptError::MalformedRow);
            }
            if message.len() != length {
                return Err(ReceiptError::MalformedRow);
            }
            if truncated > 1 {
                return Err(ReceiptError::MalformedRow);
            }
            let message = unescape_text(message).ok_or(ReceiptError::MalformedRow)?;
            if message.len() > MAX_FIELD_BYTES {
                return Err(ReceiptError::MalformedRow);
            }
            events.push(Event {
                sequence,
                message: BoundedText {
                    text: message,
                    truncated: truncated == 1,
                    original_bytes,
                },
            });
        }

        let artifact_count = next_name("artifacts")?
            .parse::<usize>()
            .map_err(|_| ReceiptError::MalformedRow)?;
        let mut artifacts = Vec::with_capacity(artifact_count);
        for _ in 0..artifact_count {
            let payload = next_name("artifact")?;
            artifacts.push(parse_length_field(&payload).ok_or(ReceiptError::MalformedRow)?);
        }
        if !rows.is_empty() {
            return Err(ReceiptError::MalformedRow);
        }

        Ok(Self {
            schema: RECEIPT_SCHEMA,
            scenario,
            seed: ScenarioSeed(seed),
            pin,
            route,
            corpus_digest,
            corpus_count,
            outcome: TerminalOutcome {
                effect,
                exit_code,
                unexecuted_reason,
            },
            comparison,
            ring_summary,
            events,
            artifacts,
        })
    }

    pub fn scenario(&self) -> &str {
        &self.scenario
    }

    pub const fn seed(&self) -> ScenarioSeed {
        self.seed
    }

    pub const fn pin(&self) -> &SourcePin {
        &self.pin
    }

    pub const fn route(&self) -> &RouteId {
        &self.route
    }

    pub const fn corpus_digest(&self) -> &ContentDigest {
        &self.corpus_digest
    }

    pub const fn corpus_count(&self) -> u64 {
        self.corpus_count
    }

    pub const fn outcome(&self) -> &TerminalOutcome {
        &self.outcome
    }

    pub const fn comparison(&self) -> Option<&ExpectedVsActual> {
        self.comparison.as_ref()
    }

    pub const fn ring_summary(&self) -> &RingSummary {
        &self.ring_summary
    }

    pub fn ring_events(&self) -> &[Event] {
        &self.events
    }

    pub fn artifacts(&self) -> &[String] {
        &self.artifacts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_cannot_forge_rows() {
        let hostile = "safe\nT 0 fake\nevent:1";
        let encoded = escape_text(hostile);
        assert!(!encoded.contains('\n'));
        assert_eq!(unescape_text(&encoded).as_deref(), Some(hostile));
    }

    #[test]
    fn adjacent_sentinels_redact_completely() {
        let redactor = Redactor::new()
            .with_sentinel("alpha")
            .with_sentinel("alphabet");
        let (long, short) = ("alphabetalphabet", "alphabet");
        assert_eq!(redactor.redact(long), format!("{REDACTED_TOKEN}{REDACTED_TOKEN}"));
        assert_eq!(redactor.redact(short), REDACTED_TOKEN);
    }

    #[test]
    fn empty_sentinels_are_ignored() {
        let redactor = Redactor::new().with_sentinel("");
        assert_eq!(redactor.redact("untouched"), "untouched");
    }

    #[test]
    fn decode_rejects_truncated_single_row() {
        let receipt = ScenarioReceipt::from_draft(
            &Redactor::new(),
            ScenarioReceiptDraft {
                scenario: "s".to_string(),
                seed: ScenarioSeed(1),
                pin: SourcePin::new(&"a".repeat(40)).unwrap(),
                route: RouteId::new("r").unwrap(),
                corpus_digest: ContentDigest::of(b"c"),
                corpus_count: 1,
                outcome: TerminalOutcome::new(Some(0), Effect::Succeeded, None),
                comparison: None,
                ring: EventRing::new(2),
                artifacts: vec![],
            },
        );
        let encoded_bytes = receipt.encode();
        let encoded_text = std::str::from_utf8(&encoded_bytes).unwrap();
        let half: String = encoded_text.lines().take(2).collect::<Vec<_>>().join("\n");
        assert_eq!(
            ScenarioReceipt::decode(half.as_bytes()),
            Err(ReceiptError::MalformedRow)
        );
    }
}
