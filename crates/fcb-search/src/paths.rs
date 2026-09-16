#![forbid(unsafe_code)]

//! Native-path navigation without loading source files (FCB-025).
//!
//! Raw root-relative paths remain authoritative. Search keys use Unicode scalar
//! values, with invalid native bytes in a disjoint token space. Insensitive
//! matching uses Rust's Unicode lowercase mapping, NOT full case folding or
//! canonical normalization. Neither key nor a display label is a filesystem
//! capability. Hosts supply already-authorized root/file identities.
//!
//! Updates publish a new immutable index. Unchanged records share their raw
//! paths and prepared keys; active membership and component postings are rebuilt
//! on the caller's worker. A failed/canceled batch leaves the old index intact.
//! Reservations conservatively retain a batch's charge while any of its records
//! survive. A fresh build from current membership can compact that overcharge.

use std::{cmp::Ordering, mem::size_of, sync::Arc};

use fcb_core::{ArenaOwnerId, ByteLength, FileId, ResourceAllocationId, ResourceBudget, ResourceLease, RootId};
use fcb_source::NativeRelativePath;
use crate::{MembershipState, SearchManifestId};

mod query;
pub use query::{PathCase, PathMatch, PathMatchKind, PathMatchMode, PathRank, PathSearch, PathSearchOptions, PathSearchState, PathSelection, PathStepBudget};

pub const PATH_KEY_VERSION: u32 = 1;
pub const MAX_PATH_BYTES: usize = 16_384;
pub const MAX_PATH_QUERY_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathSearchError {
    EmptyQuery, QueryTooLong, InvalidLimits, LimitExceeded, AllocationFailed,
    ResourceDenied, OwnerMismatch, StaleIndex, StaleQuery, DuplicateFile,
    DuplicatePath, MissingFile, InvalidUpdate, Canceled, StepBudgetTooSmall,
    SelectionUnavailable,
}
impl PathSearchError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::EmptyQuery => "PATH_QUERY_EMPTY",
            Self::QueryTooLong => "PATH_QUERY_TOO_LONG",
            Self::InvalidLimits => "PATH_INVALID_LIMITS",
            Self::LimitExceeded => "PATH_LIMIT_EXCEEDED",
            Self::AllocationFailed => "PATH_ALLOCATION_FAILED",
            Self::ResourceDenied => "PATH_RESOURCE_DENIED",
            Self::OwnerMismatch => "PATH_OWNER_MISMATCH",
            Self::StaleIndex => "PATH_STALE_INDEX",
            Self::StaleQuery => "PATH_STALE_QUERY",
            Self::DuplicateFile => "PATH_DUPLICATE_FILE",
            Self::DuplicatePath => "PATH_DUPLICATE_NATIVE_PATH",
            Self::MissingFile => "PATH_FILE_NOT_FOUND",
            Self::InvalidUpdate => "PATH_INVALID_UPDATE",
            Self::Canceled => "PATH_CANCELED",
            Self::StepBudgetTooSmall => "PATH_STEP_BUDGET_TOO_SMALL",
            Self::SelectionUnavailable => "PATH_SELECTION_UNAVAILABLE",
        }
    }
}
impl std::fmt::Display for PathSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for PathSearchError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathIndexLimits {
    pub max_files: usize,
    pub max_path_bytes: usize,
    pub max_total_path_bytes: usize,
    pub max_components: usize,
}
impl Default for PathIndexLimits {
    fn default() -> Self {
        Self { max_files: 1_000_000, max_path_bytes: MAX_PATH_BYTES,
            max_total_path_bytes: 128 * 1024 * 1024, max_components: 2_000_000 }
    }
}

/// One authorized discovery entry. File IDs must be unique within the owner;
/// roots disambiguate identical relative names in different workspaces.
#[derive(Clone, Copy, Debug)]
pub struct PathEntry<'a> {
    pub file_id: FileId,
    pub root_id: RootId,
    pub path: &'a NativeRelativePath,
    /// Host-supplied navigation preference. Larger is more recent/preferred;
    /// this never outranks a better lexical match class.
    pub recent_weight: u16,
}
impl<'a> PathEntry<'a> {
    pub fn new(file_id: FileId, root_id: RootId, path: &'a NativeRelativePath) -> Self {
        Self { file_id, root_id, path, recent_weight: 0 }
    }
}

#[derive(Debug)]
pub struct IndexedPath {
    file: FileId,
    root: RootId,
    path: NativeRelativePath,
    sensitive: Vec<u32>,
    folded: Vec<u32>,
    components: usize,
    recent: u16,
    _lease: ResourceLease,
}
impl IndexedPath {
    pub const fn file_id(&self) -> FileId { self.file }
    pub const fn root_id(&self) -> RootId { self.root }
    pub fn raw_path(&self) -> &NativeRelativePath { &self.path }
    pub const fn recent_weight(&self) -> u16 { self.recent }
}

#[derive(Clone, Copy, Debug)]
struct Component { record: usize, start: usize, end: usize, full_path: bool }

/// Sorted component postings accelerate exact/prefix lookup. Fuzzy queries
/// progressively examine file keys, not source captures or a filesystem tree.
#[derive(Debug)]
pub struct PathIndex {
    id: SearchManifestId,
    membership: MembershipState,
    records: Vec<Arc<IndexedPath>>,
    components: Vec<Component>,
    limits: PathIndexLimits,
    raw_bytes: usize,
    _lease: ResourceLease,
}
impl PathIndex {
    pub fn build(
        id: SearchManifestId, membership: MembershipState, entries: &[PathEntry<'_>],
        limits: PathIndexLimits, budget: &ResourceBudget, allocation: ResourceAllocationId,
        canceled: impl FnMut() -> bool,
    ) -> Result<Self, PathSearchError> {
        Self::publish(None, id, membership, entries, &[], limits, budget, allocation, canceled)
    }

    /// Apply sorted, disjoint upserts and explicit removals atomically. Omitted
    /// files survive, including during partial discovery. Renames keep FileId;
    /// replacement/retirement of that identity remains a host decision.
    pub fn updated(
        &self, next: SearchManifestId, membership: MembershipState,
        upserts: &[PathEntry<'_>], removed: &[FileId], budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: impl FnMut() -> bool,
    ) -> Result<Self, PathSearchError> {
        if next.owner() != self.id.owner() { return Err(PathSearchError::OwnerMismatch); }
        if next.revision() <= self.id.revision() { return Err(PathSearchError::StaleIndex); }
        Self::publish(Some(self), next, membership, upserts, removed, self.limits, budget, allocation, canceled)
    }

    pub const fn id(&self) -> SearchManifestId { self.id }
    pub const fn membership(&self) -> MembershipState { self.membership }
    pub fn len(&self) -> usize { self.records.len() }
    pub fn is_empty(&self) -> bool { self.records.is_empty() }
    pub const fn raw_path_bytes(&self) -> usize { self.raw_bytes }
    pub fn component_count(&self) -> usize { self.components.len() }
    pub fn paths(&self) -> impl ExactSizeIterator<Item = &IndexedPath> {
        self.records.iter().map(Arc::as_ref)
    }
    pub fn get(&self, file: FileId) -> Option<&IndexedPath> {
        self.records.binary_search_by_key(&file, |record| record.file)
            .ok().map(|i| self.records[i].as_ref())
    }

    #[allow(clippy::too_many_arguments)]
    fn publish(
        old: Option<&Self>, id: SearchManifestId, membership: MembershipState,
        upserts: &[PathEntry<'_>], removed: &[FileId], limits: PathIndexLimits,
        budget: &ResourceBudget, allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool,
    ) -> Result<Self, PathSearchError> {
        if limits.max_files > 1_000_000 || limits.max_path_bytes == 0
            || limits.max_path_bytes > MAX_PATH_BYTES || limits.max_components > 4_000_000 {
            return Err(PathSearchError::InvalidLimits);
        }
        if upserts.len() > limits.max_files || removed.len() > limits.max_files {
            return Err(PathSearchError::LimitExceeded);
        }
        let mut previous = None;
        let mut new_raw = 0usize;
        let mut new_units = 0usize;
        let mut components = 0usize;
        for entry in upserts {
            if canceled() { return Err(PathSearchError::Canceled); }
            validate_owner(id.owner(), entry.file_id, entry.root_id)?;
            if previous.is_some_and(|last| last >= entry.file_id) { return Err(PathSearchError::DuplicateFile); }
            if entry.path.as_bytes().len() > limits.max_path_bytes { return Err(PathSearchError::LimitExceeded); }
            previous = Some(entry.file_id);
            new_raw = add(new_raw, entry.path.as_bytes().len())?;
            let (sensitive, folded) = key_lengths(entry.path.as_bytes())?;
            new_units = add(new_units, add(sensitive, folded)?)?;
            components = add(components, component_count(entry.path.as_bytes()))?;
        }
        previous = None;
        for &file in removed {
            if canceled() { return Err(PathSearchError::Canceled); }
            if file.owner() != id.owner() { return Err(PathSearchError::OwnerMismatch); }
            if previous.is_some_and(|last| last >= file)
                || upserts.binary_search_by_key(&file, |entry| entry.file_id).is_ok() {
                return Err(PathSearchError::InvalidUpdate);
            }
            if old.and_then(|index| index.get(file)).is_none() { return Err(PathSearchError::MissingFile); }
            previous = Some(file);
        }
        let retained = |file: FileId| removed.binary_search(&file).is_err()
            && upserts.binary_search_by_key(&file, |entry| entry.file_id).is_err();
        let mut count = upserts.len();
        let mut raw_bytes = new_raw;
        if let Some(old) = old {
            for record in &old.records {
                if canceled() { return Err(PathSearchError::Canceled); }
                if retained(record.file) {
                    count = add(count, 1)?;
                    raw_bytes = add(raw_bytes, record.path.as_bytes().len())?;
                    components = add(components, record.components)?;
                }
            }
        }
        if count > limits.max_files || raw_bytes > limits.max_total_path_bytes || components > limits.max_components {
            return Err(PathSearchError::LimitExceeded);
        }
        let planned = add(size_of::<Self>(), add(
            mul(count, size_of::<Arc<IndexedPath>>())?,
            add(mul(components, size_of::<Component>())?, add(new_raw,
                add(mul(new_units, size_of::<u32>())?, mul(upserts.len(),
                    add(size_of::<IndexedPath>(), 2 * size_of::<usize>())?)?)?)?)?)?;
        if canceled() { return Err(PathSearchError::Canceled); }
        let lease = budget.try_reserve_managed(id.owner(), allocation, ByteLength::new(planned as u64))
            .map_err(|_| PathSearchError::ResourceDenied)?;
        let mut records = Vec::new();
        let mut postings = Vec::new();
        reserve(&mut records, count)?;
        reserve(&mut postings, components)?;
        if let Some(old) = old {
            for record in &old.records {
                if canceled() { return Err(PathSearchError::Canceled); }
                if retained(record.file) { records.push(Arc::clone(record)); }
            }
        }
        for entry in upserts {
            if canceled() { return Err(PathSearchError::Canceled); }
            let bytes = entry.path.as_bytes();
            let mut raw = Vec::new();
            reserve(&mut raw, bytes.len())?;
            raw.extend_from_slice(bytes);
            let (sensitive, folded) = keys(bytes)?;
            let path = NativeRelativePath::new(raw, limits.max_path_bytes)
                .map_err(|_| PathSearchError::InvalidUpdate)?;
            records.push(Arc::new(IndexedPath { file: entry.file_id, root: entry.root_id, path,
                sensitive, folded, components: component_count(bytes), recent: entry.recent_weight,
                _lease: lease.clone() }));
        }
        records.sort_unstable_by_key(|record| record.file);
        for (ordinal, record) in records.iter().enumerate() {
            if canceled() { return Err(PathSearchError::Canceled); }
            let len = record.folded.len();
            postings.push(Component { record: ordinal, start: 0, end: len, full_path: true });
            if record.components > 1 {
                let mut start = 0;
                for end in 0..=len {
                    if end == len || record.folded[end] == u32::from(b'/') {
                        postings.push(Component { record: ordinal, start, end, full_path: false });
                        start = end + 1;
                    }
                }
            }
        }
        postings.sort_unstable_by(|left, right| {
            component_key(&records, left).cmp(component_key(&records, right))
                .then_with(|| path_order(&records[left.record], &records[right.record]))
                .then_with(|| left.full_path.cmp(&right.full_path))
        });
        // Equal normalized keys are allowed, equal root/native names are not.
        let mut previous_path: Option<&IndexedPath> = None;
        for posting in postings.iter().filter(|posting| posting.full_path) {
            if canceled() { return Err(PathSearchError::Canceled); }
            let record = records[posting.record].as_ref();
            if previous_path.is_some_and(|previous|
                previous.root == record.root && previous.path == record.path) {
                return Err(PathSearchError::DuplicatePath);
            }
            previous_path = Some(record);
        }
        if canceled() { return Err(PathSearchError::Canceled); }
        Ok(Self { id, membership, records, components: postings, limits, raw_bytes, _lease: lease })
    }

    fn component_range(&self, key: &[u32], prefix: bool) -> std::ops::Range<usize> {
        let start = self.components.partition_point(|posting| component_key(&self.records, posting) < key);
        let count = self.components[start..].partition_point(|posting| {
            let candidate = component_key(&self.records, posting);
            if prefix { candidate.starts_with(key) } else { candidate == key }
        });
        start..start + count
    }
}

fn validate_owner(owner: ArenaOwnerId, file: FileId, root: RootId) -> Result<(), PathSearchError> {
    if file.owner() != owner || root.owner() != owner { return Err(PathSearchError::OwnerMismatch); }
    Ok(())
}
fn component_count(path: &[u8]) -> usize {
    let separators = path.iter().filter(|&&byte| byte == b'/').count();
    if separators == 0 { 1 } else { separators + 2 }
}
fn component_key<'a>(records: &'a [Arc<IndexedPath>], posting: &Component) -> &'a [u32] {
    &records[posting.record].folded[posting.start..posting.end]
}
fn path_order(left: &IndexedPath, right: &IndexedPath) -> Ordering {
    left.root.cmp(&right.root).then_with(|| left.path.cmp(&right.path)).then_with(|| left.file.cmp(&right.file))
}
fn add(a: usize, b: usize) -> Result<usize, PathSearchError> {
    a.checked_add(b).ok_or(PathSearchError::LimitExceeded)
}
fn mul(a: usize, b: usize) -> Result<usize, PathSearchError> {
    a.checked_mul(b).ok_or(PathSearchError::LimitExceeded)
}
fn reserve<T>(items: &mut Vec<T>, count: usize) -> Result<(), PathSearchError> {
    items.try_reserve_exact(count).map_err(|_| PathSearchError::AllocationFailed)?;
    if items.capacity() > count { return Err(PathSearchError::ResourceDenied); }
    Ok(())
}

/// Decode valid runs without replacing invalid native bytes. Values beyond the
/// Unicode scalar range cannot collide with a real character or escape label.
fn units(bytes: &[u8], mut emit: impl FnMut(u32)) {
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => { for ch in text.chars() { emit(ch as u32); } break; }
            Err(error) => {
                let valid = error.valid_up_to();
                if let Ok(text) = std::str::from_utf8(&rest[..valid]) {
                    for ch in text.chars() { emit(ch as u32); }
                }
                let bad = error.error_len().unwrap_or(rest.len() - valid);
                for &byte in &rest[valid..valid + bad] { emit(0x11_0000 + u32::from(byte)); }
                rest = &rest[valid + bad..];
            }
        }
    }
}
fn lower(unit: u32, mut emit: impl FnMut(u32)) {
    if let Some(ch) = char::from_u32(unit) {
        for lower in ch.to_lowercase() { emit(lower as u32); }
    } else { emit(unit); }
}
fn key_lengths(bytes: &[u8]) -> Result<(usize, usize), PathSearchError> {
    let mut sensitive = 0usize;
    let mut folded = 0usize;
    units(bytes, |unit| {
        sensitive = sensitive.saturating_add(1);
        lower(unit, |_| folded = folded.saturating_add(1));
    });
    if sensitive == usize::MAX || folded == usize::MAX { return Err(PathSearchError::LimitExceeded); }
    Ok((sensitive, folded))
}
fn keys(bytes: &[u8]) -> Result<(Vec<u32>, Vec<u32>), PathSearchError> {
    let (sensitive_len, folded_len) = key_lengths(bytes)?;
    let mut sensitive = Vec::new();
    let mut folded = Vec::new();
    reserve(&mut sensitive, sensitive_len)?;
    reserve(&mut folded, folded_len)?;
    units(bytes, |unit| { sensitive.push(unit); lower(unit, |value| folded.push(value)); });
    Ok((sensitive, folded))
}
