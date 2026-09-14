#![forbid(unsafe_code)]

//! Deterministic, dependency-free corpus and semantic-oracle primitives.
//!
//! These helpers are deliberately separate from the implementations they
//! exercise.  A fixture has stable source bytes, a raw path identity, measured
//! counts, and a content digest.  Reference scans and the small graph/layout
//! oracles use straightforward algorithms so an optimized consumer cannot
//! accidentally prove itself against the same implementation.

pub mod receipts;
pub mod scenario;
use std::fmt;

pub const GENERATOR_VERSION: &str = "fcb-corpus-1";
pub const DEFAULT_SEED: u64 = 0x4643_422d_324e_5a55;
const MAX_FIXTURE_BYTES: usize = 64 * 1024;
const MAX_REFERENCE_INPUT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REFERENCE_LINES: u64 = 1_000_000;
const MAX_GRAPH_NODES: usize = 128;
const MAX_GRAPH_EDGES: usize = MAX_GRAPH_NODES * MAX_GRAPH_NODES;
const MAX_LAYOUT_ITEMS: usize = 128;
const MAX_MINIMIZE_INPUT_BYTES: usize = 64 * 1024;
const MAX_RECEIPT_ATTEMPTS: u64 = 1_000_000;

/// A deterministic non-cryptographic content identity for test inputs.
///
/// The digest is an equality/change oracle, not authorization or tamper
/// resistance.  It is intentionally implemented here with no dependency so
/// the semantic oracle cannot inherit an optimized production path.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentDigest([u8; 16]);

impl ContentDigest {
    pub fn of(bytes: &[u8]) -> Self {
        let mut a = 0xcbf2_9ce4_8422_2325_u64;
        let mut b = 0x8422_2325_cbf2_9ce4_u64;
        for (index, byte) in bytes.iter().copied().enumerate() {
            a ^= u64::from(byte).wrapping_add((index as u64).rotate_left(17));
            a = a.wrapping_mul(0x0000_0100_0000_01b3);
            b ^= u64::from(byte).rotate_left((index % 63) as u32);
            b = b.wrapping_mul(0x0000_0100_0000_01b3);
        }
        a ^= bytes.len() as u64;
        b ^= (bytes.len() as u64).rotate_left(32);
        Self([
            a.to_le_bytes()[0],
            a.to_le_bytes()[1],
            a.to_le_bytes()[2],
            a.to_le_bytes()[3],
            a.to_le_bytes()[4],
            a.to_le_bytes()[5],
            a.to_le_bytes()[6],
            a.to_le_bytes()[7],
            b.to_le_bytes()[0],
            b.to_le_bytes()[1],
            b.to_le_bytes()[2],
            b.to_le_bytes()[3],
            b.to_le_bytes()[4],
            b.to_le_bytes()[5],
            b.to_le_bytes()[6],
            b.to_le_bytes()[7],
        ])
    }

    pub const fn bytes(self) -> [u8; 16] {
        self.0
    }

    pub fn hex(self) -> String {
        let mut output = String::with_capacity(32);
        for byte in self.0 {
            output.push_str(&format!("{byte:02x}"));
        }
        output
    }

    /// Parse the 32-hex-character form produced by [`Self::hex`].
    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 32 {
            return None;
        }
        let mut decoded = [0_u8; 16];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            decoded[index] = ((high << 4) | low) as u8;
        }
        Some(Self(decoded))
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.hex())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FixtureKind {
    Source,
    Path,
    Unicode,
    Tree,
    Document,
    Artifact,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureFile {
    path: Vec<u8>,
    language: &'static str,
    kind: FixtureKind,
    bytes: Vec<u8>,
    license: &'static str,
    provenance: &'static str,
    digest: ContentDigest,
}

impl FixtureFile {
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub const fn language(&self) -> &'static str {
        self.language
    }

    pub const fn kind(&self) -> FixtureKind {
        self.kind
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn license(&self) -> &'static str {
        self.license
    }

    pub const fn provenance(&self) -> &'static str {
        self.provenance
    }

    pub const fn digest(&self) -> ContentDigest {
        self.digest
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorpusManifest {
    pub seed: u64,
    pub generator_version: &'static str,
    pub file_count: u64,
    pub total_bytes: u64,
    pub digest: ContentDigest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Corpus {
    manifest: CorpusManifest,
    files: Vec<FixtureFile>,
}

impl Corpus {
    /// Build the bounded, mixed-language fixture set for `seed`.
    pub fn seeded(seed: u64) -> Self {
        let mut files = Vec::new();
        for (kind, label) in [
            (FixtureKind::Source, "source"),
            (FixtureKind::Path, "paths"),
            (FixtureKind::Unicode, "unicode"),
            (FixtureKind::Tree, "tree"),
            (FixtureKind::Document, "document"),
            (FixtureKind::Artifact, "artifact"),
        ] {
            let value = SplitMix64::new(seed ^ kind_salt(kind)).next();
            let mut path = format!("generated/{label}/{value:016x}.fixture").into_bytes();
            if kind == FixtureKind::Path {
                path = b"generated/paths/space \xce\xbb/odd-\xff.fixture".to_vec();
            }
            let content = match kind {
                FixtureKind::Unicode => format!(
                    "seed={seed:016x}\nkind={label}\nvalue={value:016x}\ntext=e\u{301} cafe\u{301} \u{1f600} \u{05d0}\n"
                )
                .into_bytes(),
                FixtureKind::Tree => b"root\n  src\n    lib.rs\n  docs\n    README.md\n".to_vec(),
                FixtureKind::Document => {
                    b"# Deterministic document\n\nA **bounded** fixture with `source` links.\n".to_vec()
                }
                FixtureKind::Artifact => {
                    let mut bytes = b"FCB1\0ARTIFACT\0".to_vec();
                    bytes.extend_from_slice(&seed.to_le_bytes());
                    bytes.extend_from_slice(&value.to_le_bytes());
                    bytes
                }
                _ => format!("seed={seed:016x}\nkind={label}\nvalue={value:016x}\n").into_bytes(),
            };
            files.push(FixtureFile::new(
                path,
                "fixture",
                kind,
                content,
                "MIT",
                "FCB-authored deterministic fixture; no external source.",
            ));
        }

        for (path, language, content) in MIXED_LANGUAGE_FILES {
            files.push(FixtureFile::new(
                path.as_bytes().to_vec(),
                language,
                FixtureKind::Source,
                content.as_bytes().to_vec(),
                "MIT",
                "FCB-authored mixed-language corpus v1; content is original and deterministic.",
            ));
        }

        let mut manifest_bytes = Vec::new();
        let mut total_bytes = 0_u64;
        for file in &files {
            manifest_bytes.extend_from_slice(&(file.path.len() as u64).to_le_bytes());
            manifest_bytes.extend_from_slice(&file.path);
            manifest_bytes.extend_from_slice(&(file.bytes.len() as u64).to_le_bytes());
            manifest_bytes.extend_from_slice(&file.bytes);
            total_bytes = total_bytes
                .checked_add(file.bytes.len() as u64)
                .expect("bounded fixture total");
        }
        let manifest = CorpusManifest {
            seed,
            generator_version: GENERATOR_VERSION,
            file_count: files.len() as u64,
            total_bytes,
            digest: ContentDigest::of(&manifest_bytes),
        };
        Self { manifest, files }
    }

    pub fn default_seeded() -> Self {
        Self::seeded(DEFAULT_SEED)
    }

    pub const fn manifest(&self) -> CorpusManifest {
        self.manifest
    }

    pub fn files(&self) -> &[FixtureFile] {
        &self.files
    }

    pub fn file(&self, path: &[u8]) -> Option<&FixtureFile> {
        self.files.iter().find(|file| file.path == path)
    }

    pub fn receipt(
        &self,
        outcome: Outcome,
        attempts: u64,
    ) -> Result<Receipt, ReceiptError> {
        if attempts > MAX_RECEIPT_ATTEMPTS {
            return Err(ReceiptError::AttemptsTooLarge);
        }
        Ok(Receipt {
            scenario: "fcb-2nzu-corpus",
            seed: self.manifest.seed,
            generator_version: self.manifest.generator_version,
            file_count: self.manifest.file_count,
            total_bytes: self.manifest.total_bytes,
            digest: self.manifest.digest,
            outcome,
            attempts,
            redacted: false,
        })
    }
}

impl FixtureFile {
    fn new(
        path: Vec<u8>,
        language: &'static str,
        kind: FixtureKind,
        bytes: Vec<u8>,
        license: &'static str,
        provenance: &'static str,
    ) -> Self {
        assert!(bytes.len() <= MAX_FIXTURE_BYTES);
        let digest = ContentDigest::of(&bytes);
        Self {
            path,
            language,
            kind,
            bytes,
            license,
            provenance,
            digest,
        }
    }
}

fn kind_salt(kind: FixtureKind) -> u64 {
    match kind {
        FixtureKind::Source => 0x11,
        FixtureKind::Path => 0x22,
        FixtureKind::Unicode => 0x33,
        FixtureKind::Tree => 0x44,
        FixtureKind::Document => 0x55,
        FixtureKind::Artifact => 0x66,
    }
}

struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

const MIXED_LANGUAGE_FILES: [(&str, &str, &str); 7] = [
    (
        "LICENSE",
        "text",
        "MIT License\nCopyright (c) 2026 FrankenCodeBrowser fixture authors\n",
    ),
    (
        "PROVENANCE.md",
        "markdown",
        "# Fixture provenance\n\nOriginal FCB-authored corpus, version 1.\n",
    ),
    (
        "src/lib.rs",
        "rust",
        "pub fn greet(name: &str) -> String { format!(\"hello {name}\") }\n",
    ),
    (
        "scripts/demo.py",
        "python",
        "def greet(name):\n    return f\"hello {name}\"\n",
    ),
    (
        "web/app.js",
        "javascript",
        "export const greet = (name) => `hello ${name}`;\n",
    ),
    (
        "tools/check.sh",
        "shell",
        "#!/bin/sh\nprintf '%s\\n' \"$1\"\n",
    ),
    (
        "config/data.yaml",
        "yaml",
        "name: corpus\nlicense: MIT\nprovenance: fcb-authored\n",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteMatch {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineSpan {
    pub number: u64,
    pub start: u64,
    pub content_end: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanError {
    EmptyNeedle,
    InputTooLarge,
    OffsetOverflow,
    TooManyLines,
    OffsetOutsideInput,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNeedle => f.write_str("empty needle"),
            Self::InputTooLarge => f.write_str("input too large"),
            Self::OffsetOverflow => f.write_str("offset overflow"),
            Self::TooManyLines => f.write_str("too many lines"),
            Self::OffsetOutsideInput => f.write_str("offset outside input"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Straightforward overlapping byte scan over one captured byte sequence.
pub fn reference_byte_scan(
    input: &[u8],
    needle: &[u8],
    max_input_bytes: u64,
) -> Result<Vec<ByteMatch>, ScanError> {
    if needle.is_empty() {
        return Err(ScanError::EmptyNeedle);
    }
    let input_len = u64::try_from(input.len()).map_err(|_| ScanError::OffsetOverflow)?;
    if input_len > max_input_bytes || input_len > MAX_REFERENCE_INPUT_BYTES {
        return Err(ScanError::InputTooLarge);
    }
    let needle_len = u64::try_from(needle.len()).map_err(|_| ScanError::OffsetOverflow)?;
    if needle_len > input_len {
        return Ok(Vec::new());
    }
    let last = input.len() - needle.len();
    let mut matches = Vec::new();
    for start in 0..=last {
        if input[start..start + needle.len()] == needle[..] {
            let start = u64::try_from(start).map_err(|_| ScanError::OffsetOverflow)?;
            let end = start.checked_add(needle_len).ok_or(ScanError::OffsetOverflow)?;
            matches.push(ByteMatch { start, end });
        }
    }
    Ok(matches)
}

/// Scan physical lines without decoding or rewriting the captured bytes.
/// CRLF is one terminator, while lone CR and LF are also line terminators.
pub fn reference_line_scan(
    input: &[u8],
    max_input_bytes: u64,
    max_lines: u64,
) -> Result<Vec<LineSpan>, ScanError> {
    let input_len = u64::try_from(input.len()).map_err(|_| ScanError::OffsetOverflow)?;
    if input_len > max_input_bytes || input_len > MAX_REFERENCE_INPUT_BYTES {
        return Err(ScanError::InputTooLarge);
    }
    let mut lines = Vec::new();
    let mut start = 0_usize;
    let mut number = 0_u64;
    while start < input.len() || (input.is_empty() && lines.is_empty()) {
        if number >= max_lines || number >= MAX_REFERENCE_LINES {
            return Err(ScanError::TooManyLines);
        }
        let mut cursor = start;
        while cursor < input.len() && input[cursor] != b'\n' && input[cursor] != b'\r' {
            cursor += 1;
        }
        let content_end = u64::try_from(cursor).map_err(|_| ScanError::OffsetOverflow)?;
        let end_index = if cursor == input.len() {
            cursor
        } else if input[cursor] == b'\r' && cursor + 1 < input.len() && input[cursor + 1] == b'\n' {
            cursor + 2
        } else {
            cursor + 1
        };
        let end = u64::try_from(end_index).map_err(|_| ScanError::OffsetOverflow)?;
        lines.push(LineSpan {
            number,
            start: u64::try_from(start).map_err(|_| ScanError::OffsetOverflow)?,
            content_end,
            end,
        });
        number = number.checked_add(1).ok_or(ScanError::OffsetOverflow)?;
        start = end_index;
    }
    if let Some(last) = lines.last() {
        if last.end > input_len {
            return Err(ScanError::OffsetOverflow);
        }
    }
    Ok(lines)
}

pub fn line_for_byte(lines: &[LineSpan], offset: u64) -> Result<u64, ScanError> {
    if lines.is_empty() || lines.last().is_some_and(|line| offset >= line.end) {
        return Err(ScanError::OffsetOutsideInput);
    }
    lines
        .iter()
        .find(|line| line.start <= offset && offset < line.end)
        .map(|line| line.number)
        .ok_or(ScanError::OffsetOutsideInput)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DefectClass {
    WrongAnswer,
    CorruptInput,
    Interrupted,
    Timeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MinimizeError {
    EmptyInput,
    InputTooLarge,
    BudgetExhausted,
    InitialDefectMismatch,
}

impl fmt::Display for MinimizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => f.write_str("empty input"),
            Self::InputTooLarge => f.write_str("input too large"),
            Self::BudgetExhausted => f.write_str("budget exhausted"),
            Self::InitialDefectMismatch => f.write_str("initial defect mismatch"),
        }
    }
}

impl std::error::Error for MinimizeError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Minimization {
    pub input: Vec<u8>,
    pub defect: DefectClass,
    pub attempts: u64,
}

/// Reduce an input only when the classifier preserves the requested defect.
/// The attempt budget is a hard bound, including the initial classification.
pub fn minimize_failure<F>(
    input: &[u8],
    defect: DefectClass,
    max_attempts: u64,
    mut classify: F,
) -> Result<Minimization, MinimizeError>
where
    F: FnMut(&[u8]) -> Option<DefectClass>,
{
    if input.is_empty() {
        return Err(MinimizeError::EmptyInput);
    }
    if input.len() > MAX_MINIMIZE_INPUT_BYTES {
        return Err(MinimizeError::InputTooLarge);
    }
    if max_attempts == 0 {
        return Err(MinimizeError::BudgetExhausted);
    }
    let mut attempts = 1_u64;
    if classify(input) != Some(defect) {
        return Err(MinimizeError::InitialDefectMismatch);
    }

    let mut current = input.to_vec();
    let mut chunk_count = 2_usize;
    while current.len() > 1 && attempts < max_attempts {
        let chunk_size = current.len().div_ceil(chunk_count);
        let mut reduced = false;
        let mut start = 0_usize;
        while start < current.len() && attempts < max_attempts {
            let end = start.saturating_add(chunk_size).min(current.len());
            if end == current.len() && start == 0 {
                break;
            }
            let mut candidate = Vec::with_capacity(current.len() - (end - start));
            candidate.extend_from_slice(&current[..start]);
            candidate.extend_from_slice(&current[end..]);
            attempts += 1;
            if classify(&candidate) == Some(defect) {
                current = candidate;
                reduced = true;
                break;
            }
            start = end;
        }
        if reduced {
            chunk_count = chunk_count.saturating_sub(1).max(2);
        } else if chunk_count >= current.len() {
            break;
        } else {
            chunk_count = chunk_count.saturating_mul(2).min(current.len());
        }
    }
    Ok(Minimization {
        input: current,
        defect,
        attempts,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphError {
    TooManyNodes,
    NodeOutOfBounds,
    DuplicateEdge,
    Cycle,
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyNodes => f.write_str("too many nodes"),
            Self::NodeOutOfBounds => f.write_str("node out of bounds"),
            Self::DuplicateEdge => f.write_str("duplicate edge"),
            Self::Cycle => f.write_str("graph cycle detected"),
        }
    }
}

impl std::error::Error for GraphError {}

/// Deterministic Kahn topological order with the smallest ready node first.
pub fn reference_topological_order(
    node_count: usize,
    edges: &[(usize, usize)],
) -> Result<Vec<usize>, GraphError> {
    if node_count > MAX_GRAPH_NODES {
        return Err(GraphError::TooManyNodes);
    }
    if edges.len() > MAX_GRAPH_EDGES {
        return Err(GraphError::TooManyNodes);
    }
    let mut outgoing = vec![Vec::new(); node_count];
    let mut indegree = vec![0_usize; node_count];
    for &(from, to) in edges {
        if from >= node_count || to >= node_count {
            return Err(GraphError::NodeOutOfBounds);
        }
        if outgoing[from].contains(&to) {
            return Err(GraphError::DuplicateEdge);
        }
        outgoing[from].push(to);
        indegree[to] = indegree[to].checked_add(1).ok_or(GraphError::Cycle)?;
    }
    let mut order = Vec::with_capacity(node_count);
    let mut used = vec![false; node_count];
    while order.len() < node_count {
        let next = (0..node_count).find(|&node| !used[node] && indegree[node] == 0);
        let node = next.ok_or(GraphError::Cycle)?;
        used[node] = true;
        order.push(node);
        for &child in &outgoing[node] {
            indegree[child] -= 1;
        }
    }
    Ok(order)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub x: u64,
    pub y: u64,
    pub width: u64,
    pub height: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutError {
    TooManyItems,
    ZeroTotalWeight,
    ArithmeticOverflow,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyItems => f.write_str("too many items"),
            Self::ZeroTotalWeight => f.write_str("zero total weight"),
            Self::ArithmeticOverflow => f.write_str("arithmetic overflow"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// A small integer reference layout: stable horizontal slices proportional
/// to weights, with the final slice receiving the exact remaining width.
pub fn reference_layout(weights: &[u64], width: u64, height: u64) -> Result<Vec<Rect>, LayoutError> {
    if weights.len() > MAX_LAYOUT_ITEMS {
        return Err(LayoutError::TooManyItems);
    }
    let total = weights.iter().try_fold(0_u64, |sum, weight| {
        sum.checked_add(*weight).ok_or(LayoutError::ArithmeticOverflow)
    })?;
    if total == 0 {
        return Err(LayoutError::ZeroTotalWeight);
    }
    let mut result = Vec::with_capacity(weights.len());
    let mut x = 0_u64;
    for (index, weight) in weights.iter().copied().enumerate() {
        let slice_width = if index + 1 == weights.len() {
            width.checked_sub(x).ok_or(LayoutError::ArithmeticOverflow)?
        } else {
            let prod = (width as u128) * (weight as u128);
            (prod / (total as u128)) as u64
        };
        result.push(Rect {
            x,
            y: 0,
            width: slice_width,
            height,
        });
        x = x
            .checked_add(slice_width)
            .ok_or(LayoutError::ArithmeticOverflow)?;
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Success,
    DeliberateFailure,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiptError {
    AttemptsTooLarge,
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AttemptsTooLarge => f.write_str("attempts count too large"),
        }
    }
}

impl std::error::Error for ReceiptError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    pub scenario: &'static str,
    pub seed: u64,
    pub generator_version: &'static str,
    pub file_count: u64,
    pub total_bytes: u64,
    pub digest: ContentDigest,
    pub outcome: Outcome,
    pub attempts: u64,
    pub redacted: bool,
}

impl Receipt {
    pub fn redacted(&self) -> Self {
        let mut receipt = self.clone();
        receipt.redacted = true;
        receipt
    }

    pub fn is_truthful_for(&self, corpus: &Corpus) -> bool {
        self.seed == corpus.manifest.seed
            && self.generator_version == corpus.manifest.generator_version
            && self.file_count == corpus.manifest.file_count
            && self.total_bytes == corpus.manifest.total_bytes
            && self.digest == corpus.manifest.digest
    }
}
