#![forbid(unsafe_code)]

//! A bounded host-declared viewport membership manifest. Paths are exact native
//! bytes, not display IDs from another atlas or permission to open new sources.
//! Validation/counting allocates nothing. A lease precedes decoded storage;
//! query replacements share immutable storage until their last reader retires.

use std::{fmt, mem::size_of, ops::Range, sync::Arc};
use fcb::store::{Sha256, Sha256Digest};
use super::{AppError, AtlasSearchError, ByteLength, ResourceAllocationId,
    ResourceBudget, ResourceLease, MAX_SEARCH_SCOPE_PATH_BYTES, reserve};

pub const MAX_SEARCH_REGION_FILES: usize = 20_000;
pub const MAX_SEARCH_REGION_WIRE_BYTES: usize = 1024 * 1024;
const OVERHEAD_BYTES: usize = 4096;

pub(super) struct VisibleRegion {
    bytes: Vec<u8>,
    paths: Vec<Range<usize>>,
    digest: Sha256Digest,
    _lease: ResourceLease,
}
impl PartialEq for VisibleRegion {
    fn eq(&self, other: &Self) -> bool { self.bytes == other.bytes && self.paths == other.paths }
}
impl Eq for VisibleRegion {}
impl fmt::Debug for VisibleRegion {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("VisibleRegion").field("files", &self.files())
            .field("path_bytes", &self.path_bytes()).field("sha256", &self.digest).finish()
    }
}

#[derive(Clone, Copy)]
struct WireShape { files: usize, bytes: usize }

impl VisibleRegion {
    pub(super) fn parse(wire: &str) -> Result<Arc<Self>, AtlasSearchError> {
        let shape = inspect(wire)?;
        // Scope construction precedes query ownership. This inert, bounded host
        // reservation follows the shared manifest through all later snapshots;
        // each query separately admits its selected FileId vector and work.
        let owner = crate::owner();
        let limit = MAX_SEARCH_REGION_WIRE_BYTES + MAX_SEARCH_REGION_FILES * size_of::<Range<usize>>()
            + size_of::<Self>() + OVERHEAD_BYTES;
        let budget = ResourceBudget::new(owner, ByteLength::new(limit as u64)).map_err(|_| AppError::Admission)?;
        let allocation = ResourceAllocationId::new(1).map_err(|_| AppError::Admission)?;
        Self::retain(wire, shape, &budget, allocation)
    }
    fn retain(wire: &str, shape: WireShape, budget: &ResourceBudget, allocation: ResourceAllocationId)
        -> Result<Arc<Self>, AtlasSearchError> {
        let charge = shape.files.checked_mul(size_of::<Range<usize>>())
            .and_then(|n| n.checked_add(shape.bytes))
            .and_then(|n| n.checked_add(size_of::<Self>() + OVERHEAD_BYTES)).ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(budget.domain(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut bytes = reserve(shape.bytes)?;
        let mut paths = reserve(shape.files)?;
        if !wire.is_empty() {
            for path in wire.split(',') {
                let start = bytes.len();
                for pair in path.as_bytes().chunks_exact(2) { bytes.push(decoded(pair)?); }
                paths.push(start..bytes.len());
            }
        }
        if paths.len() != shape.files || bytes.len() != shape.bytes { return Err(AtlasSearchError::InvalidScope); }
        Ok(Arc::new(Self { bytes, paths, digest: Sha256::digest(wire.as_bytes()), _lease: lease }))
    }
    pub(super) fn files(&self) -> usize { self.paths.len() }
    pub(super) fn path_bytes(&self) -> usize { self.bytes.len() }
    pub(super) fn digest(&self) -> Sha256Digest { self.digest }
    pub(super) fn includes(&self, path: &[u8]) -> bool {
        self.paths.binary_search_by(|range| self.bytes[range.clone()].cmp(path)).is_ok()
    }
}

fn inspect(wire: &str) -> Result<WireShape, AtlasSearchError> {
    if wire.len() > MAX_SEARCH_REGION_WIRE_BYTES { return Err(AtlasSearchError::InvalidScope); }
    let mut shape = WireShape { files: 0, bytes: 0 };
    if wire.is_empty() { return Ok(shape); }
    let mut previous: Option<&str> = None;
    for path in wire.split(',') {
        if shape.files == MAX_SEARCH_REGION_FILES || path.is_empty() || path.len() % 2 != 0
            || path.len() / 2 > MAX_SEARCH_SCOPE_PATH_BYTES
            || previous.is_some_and(|before| before >= path) {
            return Err(AtlasSearchError::InvalidScope);
        }
        let mut component_bytes = 0;
        let mut dots_only = true;
        for pair in path.as_bytes().chunks_exact(2) {
            let byte = decoded(pair)?;
            if byte == 0 { return Err(AtlasSearchError::InvalidScope); }
            if byte == b'/' {
                valid_component(component_bytes, dots_only)?;
                component_bytes = 0; dots_only = true;
            } else {
                component_bytes += 1;
                dots_only &= byte == b'.';
            }
        }
        valid_component(component_bytes, dots_only)?;
        shape.files += 1;
        shape.bytes = shape.bytes.checked_add(path.len() / 2).ok_or(AtlasSearchError::InvalidScope)?;
        previous = Some(path);
    }
    Ok(shape)
}
fn valid_component(bytes: usize, dots_only: bool) -> Result<(), AtlasSearchError> {
    if bytes == 0 || (dots_only && bytes <= 2) { Err(AtlasSearchError::InvalidScope) } else { Ok(()) }
}
fn decoded(pair: &[u8]) -> Result<u8, AtlasSearchError> {
    fn digit(byte: u8) -> Option<u8> {
        match byte { b'0'..=b'9' => Some(byte - b'0'), b'a'..=b'f' => Some(byte - b'a' + 10), _ => None }
    }
    let high = digit(pair[0]).ok_or(AtlasSearchError::InvalidScope)?;
    let low = digit(pair[1]).ok_or(AtlasSearchError::InvalidScope)?;
    Ok(high * 16 + low)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

    #[test]
    fn canonical_membership_preserves_raw_paths_and_shared_admission() {
        let wire = "612f622e7273,612f625c632e7273,ff2e6d64";
        let region = VisibleRegion::parse(wire).unwrap();
        assert_eq!(region.files(), 3);
        assert_eq!(region.path_bytes(), b"a/b.rs".len() + b"a/b\\c.rs".len() + b"\xff.md".len());
        assert_eq!(region.digest(), Sha256::digest(wire.as_bytes()));
        for path in [b"a/b.rs".as_slice(), b"a/b\\c.rs", b"\xff.md"] { assert!(region.includes(path)); }
        for path in [b"a/b/c.rs".as_slice(), b"a/b.RS", b"a", b"z.md"] { assert!(!region.includes(path)); }
        let shared = Arc::clone(&region);
        assert!(Arc::ptr_eq(&region, &shared));
        assert_eq!(*region, *VisibleRegion::parse(wire).unwrap());
        let empty = VisibleRegion::parse("").unwrap();
        assert_eq!(empty.files(), 0); assert_eq!(empty.path_bytes(), 0);
        assert_eq!(empty.digest(), Sha256::digest(b"")); assert!(!empty.includes(b"a/b.rs"));
    }

    #[test]
    fn malformed_or_noncanonical_regions_never_change_membership() {
        for wire in [",", "61,", ",61", "61,61", "62,61", "6A", "6g", "0", "00", "2f61", "612f",
            "612f2f62", "2e", "2e2e", "612f2e2f62", "612f2e2e2f62", "61 62"] {
            assert_eq!(VisibleRegion::parse(wire).err(), Some(AtlasSearchError::InvalidScope), "{wire}");
        }
        assert!(VisibleRegion::parse("2e676974,613a622e7273").is_ok());
        let composed = hex("é.rs".as_bytes()); let decomposed = hex("e\u{301}.rs".as_bytes());
        let mut paths = [composed, decomposed]; paths.sort();
        let region = VisibleRegion::parse(&paths.join(",")).unwrap();
        assert_eq!(region.files(), 2);
        assert!(region.includes("é.rs".as_bytes())); assert!(region.includes("e\u{301}.rs".as_bytes()));
    }

    #[test]
    fn region_bounds_are_independent_and_have_permitted_neighbors() {
        assert!(VisibleRegion::parse(&"61".repeat(MAX_SEARCH_SCOPE_PATH_BYTES)).is_ok());
        assert!(VisibleRegion::parse(&"61".repeat(MAX_SEARCH_SCOPE_PATH_BYTES + 1)).is_err());
        let entries: Vec<_> = (0..MAX_SEARCH_REGION_FILES).map(|index| hex(format!("{index:05}").as_bytes())).collect();
        let wire = entries.join(","); assert!(VisibleRegion::parse(&wire).is_ok());
        assert!(VisibleRegion::parse(&format!("{wire},{}", hex(b"99999"))).is_err());
        let entries: Vec<_> = (0..128).map(|index| {
            let length = if index == 127 { MAX_SEARCH_SCOPE_PATH_BYTES - 64 } else { MAX_SEARCH_SCOPE_PATH_BYTES };
            hex(format!("{index:04}{}", "x".repeat(length - 4)).as_bytes())
        }).collect();
        let wire = entries.join(",");
        assert_eq!(wire.len(), MAX_SEARCH_REGION_WIRE_BYTES - 1);
        assert!(VisibleRegion::parse(&wire).is_ok());
        assert!(VisibleRegion::parse(&(wire + "61")).is_err());
    }

    #[test]
    fn region_storage_requires_admission_before_decoding() {
        let wire = "612e7273";
        let budget = ResourceBudget::new(crate::owner(), ByteLength::new(1)).unwrap();
        assert_eq!(VisibleRegion::retain(wire, inspect(wire).unwrap(), &budget,
            ResourceAllocationId::new(1).unwrap()).err(), Some(AtlasSearchError::App(AppError::Admission)));
    }
}
