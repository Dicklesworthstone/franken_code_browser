#![forbid(unsafe_code)]

//! Explicit, bounded repository-rule observations. Reuses IgnoreMatcher's parser
//! and matching AST. A refused rule file makes its directory's policy UNKNOWN:
//! the walker must skip that subtree and publish incomplete discovery, never
//! pretend the skipped pattern was an Include answer. No rule executes commands.
//!
//! Reads use the existing regular-file admission boundary and compare the opened
//! handle with the observed file. This is NOT descriptor-relative confinement
//! against hostile ancestor replacement. Callers explicitly authorize native I/O.

use std::{fs, io::{self, Read}, mem::size_of, path::Path};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use crate::{CancelFlag, RootGrant, SourceError, safe_open_regular_file};
use super::{IgnoreLayerKind, IgnoreMatcher, NormalizedPath};
use super::budget::IgnoreWorkError;

pub const RULE_POLICY_NAME: &str = "repository-rules-v1";
const MAX_DIAGNOSTICS: usize = 32;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleLimits {
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_files: usize,
    pub max_checks: u64,
    pub max_read_calls: u64,
    pub max_rules: usize,
    pub max_pattern_bytes: usize,
    pub max_compiled_bytes: usize,
    pub max_match_steps: u64,
    pub max_total_match_steps: u64,
}
impl Default for RuleLimits {
    fn default() -> Self {
        Self { max_file_bytes: 16 * 1024, max_total_bytes: 64 * 1024, max_files: 256,
            max_checks: 32_768, max_read_calls: 65_536, max_rules: 1024,
            max_pattern_bytes: 512, max_compiled_bytes: 16 * 1024 * 1024,
            max_match_steps: 1_000_000, max_total_match_steps: 64_000_000 }
    }
}
impl RuleLimits {
    fn validate(self) -> Result<(), RuleError> {
        if self.max_file_bytes > 64 * 1024 || self.max_total_bytes > 1024 * 1024
            || self.max_files > 4096 || self.max_checks > 1_048_576 || self.max_read_calls > 1_048_576
            || self.max_rules > 8192 || self.max_pattern_bytes > 4096
            || self.max_compiled_bytes > 64 * 1024 * 1024 || self.max_match_steps > 16_000_000
            || self.max_total_match_steps > 1_000_000_000 { return Err(RuleError::Limits); }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuleError {
    Limits, ResourceDenied, CheckLimit, FileLimit, ByteLimit, ReadCallLimit,
    PatternLimit, CompileLimit, UnsupportedPattern, Encoding, Changed,
    SpecialObject, ReadFailed, MatchWork, PathLimit, Source(SourceError),
}
impl RuleError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Limits => "RULE_INVALID_LIMITS", Self::ResourceDenied => "RULE_RESOURCE_DENIED",
            Self::CheckLimit => "RULE_CHECK_LIMIT", Self::FileLimit => "RULE_FILE_LIMIT",
            Self::ByteLimit => "RULE_BYTE_LIMIT", Self::ReadCallLimit => "RULE_READ_CALL_LIMIT",
            Self::PatternLimit => "RULE_PATTERN_LIMIT", Self::CompileLimit => "RULE_COMPILE_LIMIT",
            Self::UnsupportedPattern => "RULE_UNSUPPORTED_PATTERN", Self::Encoding => "RULE_INVALID_ENCODING",
            Self::Changed => "RULE_FILE_CHANGED", Self::SpecialObject => "RULE_SPECIAL_OBJECT",
            Self::ReadFailed => "RULE_READ_FAILED", Self::MatchWork => "RULE_MATCH_WORK_LIMIT",
            Self::PathLimit => "RULE_PATH_LIMIT", Self::Source(error) => error.code(),
        }
    }
}
impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for RuleError {}
impl From<SourceError> for RuleError { fn from(error: SourceError) -> Self { Self::Source(error) } }

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuleStats {
    pub checks: u64,
    pub files_read: usize,
    pub files_loaded: usize,
    pub bytes_read: u64,
    pub read_calls: u64,
    pub rules_loaded: usize,
    pub compiled_admission_bytes: usize,
    pub match_steps: u64,
    pub failed_files: usize,
    pub unresolved_paths: usize,
    pub diagnostics_omitted: usize,
}
impl RuleStats {
    pub fn is_complete(self) -> bool { self.failed_files == 0 && self.unresolved_paths == 0 }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleDiagnostic { path: Vec<u8>, error: RuleError }
impl RuleDiagnostic {
    pub fn path(&self) -> &[u8] { &self.path }
    pub const fn error(&self) -> RuleError { self.error }
}
/// Exact successful rule observations, in native-path order. Useful for hashing
/// policy identity in an explicitly requested snapshot export. No ambient root
/// path is included. These bytes may themselves contain sensitive metadata.
pub struct RuleObservation { path: Vec<u8>, bytes: Vec<u8> }
impl RuleObservation {
    pub fn path(&self) -> &[u8] { &self.path }
    pub fn bytes(&self) -> &[u8] { &self.bytes }
}

pub struct RepositoryRules {
    limits: RuleLimits,
    matcher: IgnoreMatcher,
    stats: RuleStats,
    evidence: Vec<RuleObservation>,
    diagnostics: Vec<RuleDiagnostic>,
    owner: ArenaOwnerId,
    _lease: ResourceLease,
}
impl RepositoryRules {
    /// Construction reserves all configured compiled state, observations,
    /// diagnostic paths and bounded per-file scratch before allocating. No I/O.
    /// The private matcher cannot be enlarged through an unaccounted escape API.
    pub fn new(owner: ArenaOwnerId, limits: RuleLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, RuleError> {
        limits.validate()?;
        let charge = limits.max_compiled_bytes.checked_add(limits.max_total_bytes)
            .and_then(|n| n.checked_add(2 * limits.max_file_bytes))
            .and_then(|n| n.checked_add(limits.max_files * (size_of::<RuleObservation>() + MAX_PATH_BYTES)))
            .and_then(|n| n.checked_add(MAX_DIAGNOSTICS * (size_of::<RuleDiagnostic>() + MAX_PATH_BYTES)))
            .and_then(|n| n.checked_add((limits.max_rules + 32) * 4 * size_of::<super::CompiledPattern>()))
            .and_then(|n| n.checked_add(size_of::<Self>() + 64 * 1024)).ok_or(RuleError::Limits)?;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| RuleError::ResourceDenied)?;
        let mut matcher = IgnoreMatcher::product_defaults();
        matcher.patterns.try_reserve_exact(limits.max_rules).map_err(|_| RuleError::ResourceDenied)?;
        let mut evidence = Vec::new();
        evidence.try_reserve_exact(limits.max_files).map_err(|_| RuleError::ResourceDenied)?;
        let mut diagnostics = Vec::new();
        diagnostics.try_reserve_exact(MAX_DIAGNOSTICS).map_err(|_| RuleError::ResourceDenied)?;
        Ok(Self { limits, matcher, stats: RuleStats::default(), evidence, diagnostics, owner, _lease: lease })
    }
    pub const fn limits(&self) -> RuleLimits { self.limits }
    pub const fn stats(&self) -> RuleStats { self.stats }
    pub fn observations(&self) -> &[RuleObservation] { &self.evidence }
    pub fn diagnostics(&self) -> &[RuleDiagnostic] { &self.diagnostics }

    /// Called BEFORE enumerating this directory. Parent rules were loaded first;
    /// .fcbignore is later than .gitignore at equal depth. Sibling prefixes cannot
    /// affect one another. Excluded parents are not entered to load child rules.
    pub(crate) fn load_directory(&mut self, prefix: Option<&NormalizedPath>, resolved: &Path,
        grant: &RootGrant, cancel: &CancelFlag) -> Result<(), RuleError> {
        if grant.owner() != self.owner { return Err(SourceError::ForeignOwner.into()); }
        for name in [b".gitignore".as_slice(), b".fcbignore".as_slice()] {
            let relative = match prefix { Some(path) => path.join_segment(name), None => NormalizedPath::from_dirent_name(name) };
            let relative = match relative {
                Ok(path) if path.as_bytes().len() <= MAX_PATH_BYTES => path,
                _ => { self.fail(prefix.map_or(b"", |p| p.as_bytes()), RuleError::PathLimit, true); return Err(RuleError::PathLimit); }
            };
            if self.stats.checks == self.limits.max_checks {
                self.fail(relative.as_bytes(), RuleError::CheckLimit, true); return Err(RuleError::CheckLimit);
            }
            self.stats.checks += 1;
            let result = read_rule_file(&resolved.join(std::str::from_utf8(name).expect("fixed ASCII rule name")),
                self.limits.max_file_bytes, self.limits.max_total_bytes as u64,
                self.limits.max_read_calls, grant, cancel, &mut self.stats);
            let bytes = match result {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(error) => { self.fail(relative.as_bytes(), error, true); return Err(error); }
            };
            if let Err(error) = self.admit(prefix, relative.as_bytes(), bytes, cancel) {
                self.fail(relative.as_bytes(), error, true); return Err(error);
            }
        }
        grant.validate_active()?;
        if cancel.is_canceled() { return Err(SourceError::Canceled.into()); }
        Ok(())
    }

    /// Atomically append one observed rule file. Unsupported syntax, malformed
    /// text or a quota error never leaves a prefix of that file's rules active.
    fn admit(&mut self, prefix: Option<&NormalizedPath>, relative: &[u8], bytes: Vec<u8>,
        cancel: &CancelFlag) -> Result<(), RuleError> {
        if self.evidence.len() == self.limits.max_files { return Err(RuleError::FileLimit); }
        let text = std::str::from_utf8(&bytes).map_err(|_| RuleError::Encoding)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut lines = 0usize;
        let mut charge = 0usize;
        let prefix_cost = prefix.map_or(0, |p| p.as_bytes().len() * 4 + p.segments().len() * 128);
        for raw in text.split('\n') {
            if cancel.is_canceled() { return Err(SourceError::Canceled.into()); }
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            if raw.len() > self.limits.max_pattern_bytes || raw.as_bytes().contains(&0) { return Err(RuleError::PatternLimit); }
            let raw = super::strip_unescaped_trailing_spaces(raw);
            if raw.is_empty() || raw.starts_with('#') { continue; }
            // These POSIX class/collating forms are not in the existing compiler's
            // qualified subset; do not reinterpret them as a different byte class.
            if raw.contains("[[:") || raw.contains("[[.") || raw.contains("[[=") { return Err(RuleError::UnsupportedPattern); }
            lines += 1;
            charge = charge.checked_add((raw.len() + 1) * 512 + prefix_cost + 512).ok_or(RuleError::CompileLimit)?;
        }
        if lines > self.limits.max_rules.saturating_sub(self.stats.rules_loaded) { return Err(RuleError::PatternLimit); }
        if charge > self.limits.max_compiled_bytes.saturating_sub(self.stats.compiled_admission_bytes) { return Err(RuleError::CompileLimit); }
        // Conservative per-byte charge covers class expansion, prefix clones,
        // AST vectors/strings and candidate-versus-retained allocation overlap.
        let mut candidate = IgnoreMatcher::include_all();
        candidate.patterns.try_reserve_exact(lines).map_err(|_| RuleError::ResourceDenied)?;
        candidate.add_rule_file(prefix, text);
        if !candidate.unsupported.is_empty() { return Err(RuleError::UnsupportedPattern); }
        if cancel.is_canceled() { return Err(SourceError::Canceled.into()); }
        self.matcher.patterns.extend(candidate.patterns);
        self.stats.compiled_admission_bytes += charge;
        self.stats.rules_loaded += lines;
        self.stats.files_loaded += 1;
        let path = relative.to_vec();
        let position = self.evidence.binary_search_by(|entry| entry.path.as_slice().cmp(relative)).unwrap_or_else(|i| i);
        self.evidence.insert(position, RuleObservation { path, bytes });
        Ok(())
    }

    pub(crate) fn excluded(&mut self, path: &NormalizedPath, is_dir: bool) -> Result<bool, RuleError> {
        let allowance = self.limits.max_match_steps.min(self.limits.max_total_match_steps.saturating_sub(self.stats.match_steps));
        let mut remaining = allowance;
        let decision = self.matcher.decide_bounded(path, is_dir, &mut remaining);
        self.stats.match_steps += allowance - remaining;
        match decision {
            Ok(decision) => Ok(decision.is_excluded()),
            Err(error) => {
                let error = match error { IgnoreWorkError::PathLimit => RuleError::PathLimit, IgnoreWorkError::WorkLimit => RuleError::MatchWork };
                self.fail(path.as_bytes(), error, false); Err(error)
            }
        }
    }
    fn fail(&mut self, path: &[u8], error: RuleError, file: bool) {
        if file { self.stats.failed_files += 1; } else { self.stats.unresolved_paths += 1; }
        if self.diagnostics.len() == MAX_DIAGNOSTICS || path.len() > MAX_PATH_BYTES {
            self.stats.diagnostics_omitted += 1;
        } else { self.diagnostics.push(RuleDiagnostic { path: path.to_vec(), error }); }
    }
}

/// Shared by explicit policy discovery and the legacy per-file loader. A
/// metadata-size check NEVER authorizes an unbounded fs::read/read_to_end.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_rule_file(path: &Path, max_file_bytes: usize, max_total_bytes: u64,
    max_calls: u64, grant: &RootGrant, cancel: &CancelFlag, stats: &mut RuleStats)
    -> Result<Option<Vec<u8>>, RuleError> {
    grant.validate_active()?;
    if cancel.is_canceled() { return Err(SourceError::Canceled.into()); }
    let observed = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RuleError::ReadFailed),
    };
    if !observed.file_type().is_file() { return Err(RuleError::SpecialObject); }
    let length = usize::try_from(observed.len()).map_err(|_| RuleError::ByteLimit)?;
    if length > max_file_bytes || observed.len() > max_total_bytes.saturating_sub(stats.bytes_read) { return Err(RuleError::ByteLimit); }
    let mut file = safe_open_regular_file(path)?;
    let opened = file.metadata().map_err(|_| RuleError::ReadFailed)?;
    if !same_file(&observed, &opened) || opened.len() != observed.len() { return Err(RuleError::Changed); }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| RuleError::ResourceDenied)?;
    if bytes.capacity() > length { return Err(RuleError::ResourceDenied); }
    bytes.resize(length, 0);
    let mut offset = 0;
    loop {
        grant.validate_active()?;
        if cancel.is_canceled() { return Err(SourceError::Canceled.into()); }
        if stats.read_calls == max_calls { return Err(RuleError::ReadCallLimit); }
        stats.read_calls += 1;
        let end = length.min(offset + 4096);
        let mut lookahead = [0u8; 1];
        let destination = if offset == length { &mut lookahead[..] } else { &mut bytes[offset..end] };
        match file.read(destination) {
            Ok(0) if offset == length => break,
            Ok(0) => return Err(RuleError::Changed),
            Ok(n) if n <= destination.len() => {
                stats.bytes_read = stats.bytes_read.saturating_add(n as u64);
                if offset == length || stats.bytes_read > max_total_bytes { return Err(RuleError::Changed); }
                offset += n;
            }
            Ok(_) => return Err(RuleError::ReadFailed),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(RuleError::ReadFailed),
        }
    }
    let after = file.metadata().map_err(|_| RuleError::ReadFailed)?;
    if after.len() != opened.len() || matches!((opened.modified().ok(), after.modified().ok()), (Some(a), Some(b)) if a != b) {
        return Err(RuleError::Changed);
    }
    grant.validate_active()?;
    if cancel.is_canceled() { return Err(SourceError::Canceled.into()); }
    stats.files_read += 1;
    Ok(Some(bytes))
}
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt; a.dev() == b.dev() && a.ino() == b.ino() }
    #[cfg(not(unix))] { let _ = (a, b); true }
}
