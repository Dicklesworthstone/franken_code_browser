#![forbid(unsafe_code)]

//! First-party ignore matching, default exclusions, nested rule precedence,
//! and deliberate browse overrides (FCB-010.B / fcb-hh2.2).
//!
//! Exclusion is classification, never deletion. An ignored path remains an
//! observed namespace entry so user annotations and deliberate browse
//! overrides can still name it. Unsupported patterns are retained as reports
//! rather than guessed. [`repository`] adds an explicit bounded file-loading
//! policy; ordinary matcher construction never performs I/O.

pub mod budget;
pub mod repository;
use crate::path::NormalizedPath;

/// Why a pattern could not be compiled into the supported Git-style subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedReason {
    /// A character class was opened with `[` and never closed.
    UnclosedClass,
    /// A trailing backslash escaped nothing.
    DanglingEscape,
    /// The line was empty after stripping a lone `!` or `/`.
    EmptyPattern,
}

/// One pattern the matcher refused to guess at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsupportedPattern {
    raw: String,
    layer: IgnoreLayerKind,
    reason: UnsupportedReason,
}
impl UnsupportedPattern {
    pub fn raw(&self) -> &str { &self.raw }
    pub fn layer(&self) -> IgnoreLayerKind { self.layer }
    pub fn reason(&self) -> UnsupportedReason { self.reason }
}

/// Origin of one compiled rule or unsupported report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnoreLayerKind {
    DefaultPolicy,
    RuleFile,
    Scope,
}
/// Why a path was classified as excluded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExclusionCause { DefaultPolicy, RuleFile, Scope }
/// Classification of one relative path. Exclusion is not a tombstone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnoreDecision { Include, Exclude { cause: ExclusionCause } }
impl IgnoreDecision {
    pub const fn is_excluded(self) -> bool { matches!(self, Self::Exclude { .. }) }
}

#[derive(Clone, Debug)]
enum SegAtom {
    Lit(Vec<u8>), Star, Ques,
    Class { negated: bool, bytes: Vec<u8> },
}
#[derive(Clone, Debug)]
enum SegPat { GlobStar, Atoms(Vec<SegAtom>) }
#[derive(Clone, Debug)]
struct CompiledPattern {
    #[allow(dead_code)]
    raw: String,
    layer: IgnoreLayerKind,
    negated: bool,
    directory_only: bool,
    basename_only: bool,
    /// Prefix relative to the scan root that this layer is anchored in.
    layer_prefix: Vec<Vec<u8>>,
    segs: Vec<SegPat>,
}

/// First-party ignore engine with ordered layers and browse overrides.
#[derive(Clone, Debug)]
pub struct IgnoreMatcher {
    patterns: Vec<CompiledPattern>,
    unsupported: Vec<UnsupportedPattern>,
    overrides: Vec<Vec<Vec<u8>>>,
    excluded: u64,
}
impl IgnoreMatcher {
    /// Match nothing; construction does not load repository policy files.
    pub fn include_all() -> Self {
        Self { patterns: Vec::new(), unsupported: Vec::new(), overrides: Vec::new(), excluded: 0 }
    }
    /// Product defaults from plan §8.2.
    pub fn product_defaults() -> Self {
        let mut matcher = Self::include_all(); matcher.push_defaults(); matcher
    }
    fn push_defaults(&mut self) {
        const DEFAULTS: &[&str] = &[
            ".git/", ".hg/", ".svn/", "node_modules/", "target/", "dist/", "build/",
            ".venv/", "__pycache__/", "*.pyc", "*.o", "*.a", "*.so", "*.dylib", "*.dll", "*.exe", "*.class",
        ];
        for raw in DEFAULTS { self.add_raw(IgnoreLayerKind::DefaultPolicy, &[], raw); }
    }
    /// Patterns are relative to the containing directory; None means the root.
    /// Both LF and CRLF rule files use the same literal pattern bytes.
    pub fn add_rule_file(&mut self, dir_prefix: Option<&NormalizedPath>, text: &str) {
        let prefix: Vec<Vec<u8>> = dir_prefix.map(|path| path.segments().iter()
            .map(|segment| segment.as_bytes().to_vec()).collect()).unwrap_or_default();
        for line in text.split('\n') {
            self.add_raw(IgnoreLayerKind::RuleFile, &prefix, line.strip_suffix('\r').unwrap_or(line));
        }
    }
    /// Add one extra application/host scope rule relative to the scan root.
    pub fn add_scope_rule(&mut self, raw: &str) { self.add_raw(IgnoreLayerKind::Scope, &[], raw); }
    /// Override default/scope exclusions, but still honor nested rule files.
    pub fn override_browse(&mut self, path: &NormalizedPath) {
        let segs: Vec<Vec<u8>> = path.segments().iter().map(|seg| seg.as_bytes().to_vec()).collect();
        if !self.overrides.iter().any(|existing| existing == &segs) { self.overrides.push(segs); }
    }
    pub fn unsupported(&self) -> &[UnsupportedPattern] { &self.unsupported }
    pub fn excluded_count(&self) -> u64 { self.excluded }
    /// Compatibility API with iterative matching. Use decide_bounded when a
    /// consumer needs an explicit work allowance and an unknown-on-limit result.
    pub fn decide(&mut self, path: &NormalizedPath, is_dir: bool) -> IgnoreDecision {
        let decision = self.peek(path, is_dir);
        if decision.is_excluded() { self.excluded = self.excluded.saturating_add(1); }
        decision
    }
    /// Non-mutating decision; does not increment excluded_count.
    pub fn peek(&self, path: &NormalizedPath, is_dir: bool) -> IgnoreDecision {
        let segs: Vec<&[u8]> = path.segments().iter().map(|seg| seg.as_bytes()).collect();
        let overridden = self.is_overridden(&segs);
        let mut last = None;
        for pattern in &self.patterns {
            if overridden && matches!(pattern.layer, IgnoreLayerKind::DefaultPolicy | IgnoreLayerKind::Scope) { continue; }
            if pattern.matches(&segs, is_dir) { last = Some((pattern.negated, pattern.layer)); }
        }
        match last {
            Some((false, layer)) => IgnoreDecision::Exclude { cause: match layer {
                IgnoreLayerKind::DefaultPolicy => ExclusionCause::DefaultPolicy,
                IgnoreLayerKind::RuleFile => ExclusionCause::RuleFile,
                IgnoreLayerKind::Scope => ExclusionCause::Scope,
            } },
            _ => IgnoreDecision::Include,
        }
    }
    fn is_overridden(&self, segs: &[&[u8]]) -> bool {
        self.overrides.iter().any(|prefix| segs.len() >= prefix.len()
            && segs.iter().zip(prefix.iter()).all(|(a, b)| *a == b.as_slice()))
    }
    fn add_raw(&mut self, layer: IgnoreLayerKind, prefix: &[Vec<u8>], raw_line: &str) {
        let trimmed = strip_unescaped_trailing_spaces(raw_line);
        if trimmed.is_empty() || trimmed.starts_with('#') { return; }
        match compile_pattern(layer, prefix, trimmed) {
            Ok(pattern) => self.patterns.push(pattern),
            Err(reason) => self.unsupported.push(UnsupportedPattern { raw: trimmed.to_string(), layer, reason }),
        }
    }
}

fn strip_unescaped_trailing_spaces(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b' ' {
        let escaped = end >= 2 && bytes[end - 2] == b'\\' && !is_escaped(bytes, end - 2);
        if escaped { break; }
        end -= 1;
    }
    &line[..end] // Only ASCII spaces removed, so this remains a UTF-8 boundary.
}
fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let (mut slashes, mut i) = (0usize, index);
    while i > 0 && bytes[i - 1] == b'\\' { slashes += 1; i -= 1; }
    slashes % 2 == 1
}
fn compile_pattern(layer: IgnoreLayerKind, prefix: &[Vec<u8>], raw: &str)
    -> Result<CompiledPattern, UnsupportedReason> {
    let mut rest = raw;
    let negated = if let Some(stripped) = rest.strip_prefix('!') { rest = stripped; true } else { false };
    let directory_only = rest.ends_with('/');
    if directory_only { rest = &rest[..rest.len() - 1]; }
    if rest.is_empty() { return Err(UnsupportedReason::EmptyPattern); }
    let rooted = rest.starts_with('/');
    if rooted { rest = &rest[1..]; }
    if rest.is_empty() { return Err(UnsupportedReason::EmptyPattern); }
    let basename_only = !rooted && !rest.as_bytes().contains(&b'/');
    let segs = compile_segments(rest.as_bytes())?;
    Ok(CompiledPattern { raw: raw.to_string(), layer, negated, directory_only,
        basename_only, layer_prefix: prefix.to_vec(), segs })
}
fn compile_segments(bytes: &[u8]) -> Result<Vec<SegPat>, UnsupportedReason> {
    let mut segs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' { i += 1; continue; }
        if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let at_seg_start = i == 0 || bytes[i - 1] == b'/';
            let at_seg_end = i + 2 == bytes.len() || bytes[i + 2] == b'/';
            if at_seg_start && at_seg_end { segs.push(SegPat::GlobStar); i += 2; continue; }
        }
        let (atoms, next) = compile_segment_atoms(bytes, i)?;
        segs.push(SegPat::Atoms(atoms)); i = next;
    }
    if segs.is_empty() { return Err(UnsupportedReason::EmptyPattern); }
    Ok(segs)
}
fn compile_segment_atoms(bytes: &[u8], start: usize) -> Result<(Vec<SegAtom>, usize), UnsupportedReason> {
    let mut atoms = Vec::new();
    let mut lit = Vec::new();
    let mut i = start;
    let flush_lit = |lit: &mut Vec<u8>, atoms: &mut Vec<SegAtom>| {
        if !lit.is_empty() { atoms.push(SegAtom::Lit(std::mem::take(lit))); }
    };
    while i < bytes.len() && bytes[i] != b'/' {
        match bytes[i] {
            b'\\' => {
                if i + 1 >= bytes.len() { return Err(UnsupportedReason::DanglingEscape); }
                lit.push(bytes[i + 1]); i += 2;
            }
            b'*' => {
                flush_lit(&mut lit, &mut atoms);
                while i < bytes.len() && bytes[i] == b'*' { i += 1; }
                atoms.push(SegAtom::Star);
            }
            b'?' => { flush_lit(&mut lit, &mut atoms); atoms.push(SegAtom::Ques); i += 1; }
            b'[' => {
                flush_lit(&mut lit, &mut atoms);
                let (class, next) = compile_class(bytes, i)?;
                atoms.push(class); i = next;
            }
            other => { lit.push(other); i += 1; }
        }
    }
    flush_lit(&mut lit, &mut atoms);
    Ok((atoms, i))
}
fn compile_class(bytes: &[u8], start: usize) -> Result<(SegAtom, usize), UnsupportedReason> {
    let mut i = start + 1;
    if i >= bytes.len() { return Err(UnsupportedReason::UnclosedClass); }
    let negated = bytes[i] == b'!' || bytes[i] == b'^';
    if negated { i += 1; }
    let mut class_bytes = Vec::new();
    let mut closed = false;
    while i < bytes.len() && bytes[i] != b'/' {
        if bytes[i] == b'\\' {
            if i + 1 >= bytes.len() { return Err(UnsupportedReason::DanglingEscape); }
            class_bytes.push(bytes[i + 1]); i += 2; continue;
        }
        if bytes[i] == b']' && !class_bytes.is_empty() { closed = true; i += 1; break; }
        if i + 2 < bytes.len() && bytes[i + 1] == b'-' && bytes[i + 2] != b']' {
            let (lo, hi) = (bytes[i], bytes[i + 2]);
            let (start_b, end_b) = if lo <= hi { (lo, hi) } else { (hi, lo) };
            for b in start_b..=end_b { class_bytes.push(b); }
            i += 3; continue;
        }
        class_bytes.push(bytes[i]); i += 1;
    }
    if !closed { return Err(UnsupportedReason::UnclosedClass); }
    Ok((SegAtom::Class { negated, bytes: class_bytes }, i))
}
impl CompiledPattern {
    fn matches(&self, path: &[&[u8]], is_dir: bool) -> bool {
        // The compatibility interface has no unknown result. Its admitted input
        // is matched with the same iterative engine, without a practical work cap.
        budget::pattern_matches(self, path, is_dir, &mut u64::MAX).unwrap_or(false)
    }
}
