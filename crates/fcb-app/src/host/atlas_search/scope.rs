#![forbid(unsafe_code)]

//! Frozen query admission, independent of the atlas's display-only filter.
//! Match native path bytes so undecodable parent/basename bytes do not hide an
//! otherwise valid ASCII extension. No source is opened while selecting files.

use super::*;
use std::sync::Arc;
mod region;
pub use region::{MAX_SEARCH_REGION_FILES, MAX_SEARCH_REGION_WIRE_BYTES};
use region::VisibleRegion;

/// Membership counts use the entire requested region, independently of the
/// file-type predicate. Unknown requested paths never become successful misses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RegionCoverage { pub catalogued: usize, pub missing: usize }

pub const MAX_SEARCH_SCOPE_TOKENS: usize = 32;
pub const MAX_SEARCH_SCOPE_TOKEN_BYTES: usize = 64;
pub const MAX_SEARCH_SCOPE_LIST_BYTES: usize = 1024;
pub const MAX_SEARCH_SCOPE_PATH_BYTES: usize = 4096;

/// Path predicates narrow the existing authorized catalog; they never resolve
/// a path, create a grant, or normalize two distinct native filenames together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasSearchPathKind { Workspace, Directory, File, Visible }
impl AtlasSearchPathKind {
    pub const fn name(self) -> &'static str {
        match self { Self::Workspace => "workspace", Self::Directory => "directory", Self::File => "file", Self::Visible => "visible" }
    }
}

// Inline storage avoids unaccounted parser allocations before query admission.
// Its retained bytes are included in size_of::<Snapshot/SearchWork> reservations;
// bridge commands borrow the scope instead of enlarging their command enums.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PathScope {
    kind: AtlasSearchPathKind,
    length: usize,
    bytes: [u8; MAX_SEARCH_SCOPE_PATH_BYTES],
}
impl PathScope {
    const WORKSPACE: Self = Self { kind: AtlasSearchPathKind::Workspace, length: 0,
        bytes: [0; MAX_SEARCH_SCOPE_PATH_BYTES] };
    fn parse(kind: AtlasSearchPathKind, path: &str) -> Result<Self, AtlasSearchError> {
        if kind == AtlasSearchPathKind::Visible || path.len() > MAX_SEARCH_SCOPE_PATH_BYTES || path.as_bytes().contains(&0) {
            return Err(AtlasSearchError::InvalidScope);
        }
        if kind == AtlasSearchPathKind::Workspace {
            return if path.is_empty() { Ok(Self::WORKSPACE) } else { Err(AtlasSearchError::InvalidScope) };
        }
        // Leading/trailing/repeated separators and dot components have no
        // alternate interpretation. Backslash is an ordinary native filename
        // byte on this Unix catalog, not an invented Windows path separator.
        if path.split('/').any(|component| matches!(component, "" | "." | "..")) {
            return Err(AtlasSearchError::InvalidScope);
        }
        let mut result = Self::WORKSPACE;
        result.kind = kind; result.length = path.len();
        result.bytes[..path.len()].copy_from_slice(path.as_bytes());
        Ok(result)
    }
    fn path(&self) -> &[u8] { &self.bytes[..self.length] }
    fn includes(&self, candidate: &[u8]) -> bool {
        match self.kind {
            AtlasSearchPathKind::Workspace | AtlasSearchPathKind::Visible => true,
            AtlasSearchPathKind::File => candidate == self.path(),
            AtlasSearchPathKind::Directory => candidate.starts_with(self.path())
                && candidate.get(self.length) == Some(&b'/'),
        }
    }
    fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal(",\"scope_path_kind\":")?; out.quoted(self.kind.name())?;
        out.literal(",\"scope_path\":")?;
        out.quoted(std::str::from_utf8(self.path()).map_err(|_| AtlasSearchError::InvalidScope)?)?;
        out.literal(",\"scope_path_hex\":")?; out.hex(self.path())?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Tokens {
    count: usize,
    lengths: [u8; MAX_SEARCH_SCOPE_TOKENS],
    bytes: [[u8; MAX_SEARCH_SCOPE_TOKEN_BYTES]; MAX_SEARCH_SCOPE_TOKENS],
}
impl Tokens {
    const EMPTY: Self = Self { count: 0, lengths: [0; MAX_SEARCH_SCOPE_TOKENS],
        bytes: [[0; MAX_SEARCH_SCOPE_TOKEN_BYTES]; MAX_SEARCH_SCOPE_TOKENS] };

    fn parse(csv: &str, filename: bool) -> Result<Self, AtlasSearchError> {
        if csv.len() > MAX_SEARCH_SCOPE_LIST_BYTES { return Err(AtlasSearchError::InvalidScope); }
        let mut result = Self::EMPTY;
        if csv.is_empty() { return Ok(result); }
        for token in csv.split(',') {
            if token.is_empty() || token.len() > MAX_SEARCH_SCOPE_TOKEN_BYTES
                || (filename && matches!(token, "." | ".."))
                || !token.bytes().all(|b| b.is_ascii_alphanumeric()
                    || matches!(b, b'_' | b'+' | b'-') || (filename && b == b'.')) {
                return Err(AtlasSearchError::InvalidScope);
            }
            if result.contains(token.as_bytes()) { continue; }
            if result.count == MAX_SEARCH_SCOPE_TOKENS { return Err(AtlasSearchError::InvalidScope); }
            let slot = result.count;
            result.lengths[slot] = token.len() as u8;
            for (target, byte) in result.bytes[slot].iter_mut().zip(token.bytes()) {
                *target = byte.to_ascii_lowercase();
            }
            result.count += 1;
            // Bounded inline insertion sort provides a stable canonical echo
            // without allocating temporary strings or a token vector.
            let mut position = slot;
            while position > 0 && result.token(position) < result.token(position - 1) {
                result.bytes.swap(position, position - 1);
                result.lengths.swap(position, position - 1);
                position -= 1;
            }
        }
        Ok(result)
    }
    fn token(&self, index: usize) -> &[u8] { &self.bytes[index][..self.lengths[index] as usize] }
    fn contains(&self, candidate: &[u8]) -> bool {
        (0..self.count).any(|index| self.token(index).eq_ignore_ascii_case(candidate))
    }
    fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal("\"")?;
        for index in 0..self.count {
            if index > 0 { out.literal(",")?; }
            // Validation restricts tokens to ASCII without JSON metacharacters.
            let text = std::str::from_utf8(self.token(index)).map_err(|_| AtlasSearchError::InvalidScope)?;
            out.literal(text)?;
        }
        out.literal("\"")?;
        Ok(())
    }
}

/// An explicit file-type query scope. Both absent lists select all files; any
/// present list, even empty, selects the union of its extensions and basenames.
/// Extensions omit the dot; ASCII case is ignored. Scope never grants access to
/// a file outside the atlas's original frozen catalog. An optional directory or
/// exact-file or host-declared visible-region predicate intersects these types
/// before any source/query quota. Region storage is immutable and separately
/// admitted; cloning a scope shares that storage without recopying path bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AtlasSearchScope {
    all: bool,
    extensions: Tokens,
    filenames: Tokens,
    path: PathScope,
    region: Option<Arc<VisibleRegion>>,
    case_sensitive: bool,
}
impl Default for AtlasSearchScope { fn default() -> Self { Self::all() } }
impl AtlasSearchScope {
    pub const fn all() -> Self { Self { all: true, extensions: Tokens::EMPTY,
        filenames: Tokens::EMPTY, path: PathScope::WORKSPACE, region: None, case_sensitive: true } }
    /// Text matching is part of the frozen query, independent of file membership.
    /// False selects the shared engine's Unicode lowercase literal semantics;
    /// it does not normalize paths, grant access, or change structural name search.
    pub const fn with_case_sensitive(mut self, case_sensitive: bool) -> Self {
        self.case_sensitive = case_sensitive;
        self
    }
    pub const fn case_sensitive(&self) -> bool { self.case_sensitive }
    pub const fn is_all(&self) -> bool { self.all && matches!(self.path.kind, AtlasSearchPathKind::Workspace) }
    pub fn from_csv(extensions: Option<&str>, filenames: Option<&str>) -> Result<Self, AtlasSearchError> {
        if extensions.is_none() && filenames.is_none() { return Ok(Self::all()); }
        Ok(Self { all: false, extensions: Tokens::parse(extensions.unwrap_or(""), false)?,
            filenames: Tokens::parse(filenames.unwrap_or(""), true)?, path: PathScope::WORKSPACE,
            region: None, case_sensitive: true })
    }
    /// Freeze a validated UTF-8 relative path without Unicode/case normalization
    /// or filesystem I/O. Directory paths omit their trailing separator and
    /// match descendants at a component boundary. Empty path means workspace
    /// only; no missing or invalid path broadens a directory/file request.
    /// Visible instead accepts a canonical lowercase hex CSV of exact raw
    /// paths, sorted and unique. Empty CSV is an explicit empty visible region.
    /// Its bounded immutable allocation is leased before decoding and shared by
    /// cloned scopes. This host-declared membership is not native frame proof.
    pub fn with_path(mut self, kind: AtlasSearchPathKind, path: &str) -> Result<Self, AtlasSearchError> {
        if kind == AtlasSearchPathKind::Visible {
            let region = VisibleRegion::parse(path)?;
            self.path = PathScope { kind, ..PathScope::WORKSPACE };
            self.region = Some(region);
        } else {
            self.path = PathScope::parse(kind, path)?;
            self.region = None;
        }
        Ok(self)
    }
    /// Declared viewport membership, including paths absent from a search
    /// catalog. This is independent of any additional file-type filtering.
    pub fn region_files(&self) -> Option<usize> { self.region.as_ref().map(|region| region.files()) }
    pub fn includes_path(&self, path: &[u8]) -> bool {
        if !self.path.includes(path) || self.region.as_ref().is_some_and(|region| !region.includes(path)) { return false; }
        if self.all { return true; }
        let basename = path.rsplit(|&byte| byte == b'/').next().unwrap_or(path);
        if self.filenames.contains(basename) { return true; }
        let Some(dot) = basename.iter().rposition(|&byte| byte == b'.') else { return false; };
        dot != 0 && dot + 1 < basename.len() && self.extensions.contains(&basename[dot + 1..])
    }
    pub(super) fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal(",\"search_scope\":")?; out.quoted(if self.all { "all" } else { "file-types" })?;
        out.literal(",\"scope_extensions\":")?; self.extensions.encode(out)?;
        out.literal(",\"scope_filenames\":")?; self.filenames.encode(out)?;
        self.path.encode(out)?;
        if let Some(region) = &self.region {
            out.literal(",\"scope_region_sha256\":")?; out.quoted(&region.digest().to_hex())?;
            out.literal(",\"scope_region_files\":")?; out.integer(region.files() as u64)?;
            out.literal(",\"scope_region_path_bytes\":")?; out.integer(region.path_bytes() as u64)?;
        }
        out.literal(",\"case_sensitive\":")?; out.boolean(self.case_sensitive)?;
        Ok(())
    }
    pub(super) fn admission_counts(&self, atlas: &AtlasSession, canceled: &mut impl FnMut() -> bool)
        -> Result<(usize, Option<RegionCoverage>), AtlasSearchError> {
        if self.is_all() { return Ok((atlas.atlas().file_count(), None)); }
        let (mut eligible, mut catalogued) = (0, 0);
        for entry in atlas.atlas().catalog().entries() {
            check(canceled)?;
            let path = entry.path().raw().as_bytes();
            if self.region.as_ref().is_some_and(|region| region.includes(path)) { catalogued += 1; }
            if self.includes_path(path) { eligible += 1; }
        }
        let region = self.region.as_ref().map(|region| {
            let missing = region.files().checked_sub(catalogued).ok_or(AtlasSearchError::WrongAtlas)?;
            Ok::<_, AtlasSearchError>(RegionCoverage { catalogued, missing })
        }).transpose()?;
        Ok((eligible, region))
    }
    /// Caller reserves the exact selected-ID capacity before this allocation.
    /// All retains the legacy catalog cursor without a duplicate universe vector.
    pub(super) fn freeze(&self, atlas: &AtlasSession, eligible: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<Vec<FileId>>, AtlasSearchError> {
        if self.is_all() { return Ok(None); }
        let catalog = atlas.atlas().catalog();
        let mut files = reserve(eligible)?;
        for (ordinal, entry) in catalog.entries().iter().enumerate() {
            check(canceled)?;
            if self.includes_path(entry.path().raw().as_bytes()) {
                if files.len() == eligible { return Err(AtlasSearchError::WrongAtlas); }
                files.push(catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?);
            }
        }
        if files.len() != eligible { return Err(AtlasSearchError::WrongAtlas); }
        files.sort_unstable();
        Ok(Some(files))
    }
}

pub(super) fn includes_file(files: &Option<Vec<FileId>>, file: FileId) -> bool {
    files.as_ref().is_none_or(|files| files.binary_search(&file).is_ok())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_manifest_is_exact_intersection_and_never_implicit_all() {
        let wire = "7372632f612e5253,7372632f622e6d64";
        let visible = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
            .with_path(AtlasSearchPathKind::Visible, wire).unwrap();
        assert!(!visible.is_all()); assert_eq!(visible.region_files(), Some(2));
        assert!(visible.includes_path(b"src/a.RS"));
        assert!(!visible.includes_path(b"src/a.rs"));
        assert!(!visible.includes_path(b"src/b.md"));
        assert!(!visible.includes_path(b"elsewhere.rs"));
        let same = AtlasSearchScope::from_csv(Some("RS"), None).unwrap()
            .with_path(AtlasSearchPathKind::Visible, wire).unwrap();
        assert_eq!(visible, same); // Independent leases do not change identity.
        let shared = visible.clone().with_case_sensitive(false);
        assert!(Arc::ptr_eq(visible.region.as_ref().unwrap(), shared.region.as_ref().unwrap()));
        assert!(shared.includes_path(b"src/a.RS")); assert!(!shared.case_sensitive());
        let workspace = visible.with_path(AtlasSearchPathKind::Workspace, "").unwrap();
        assert_eq!(workspace.region_files(), None); assert!(workspace.includes_path(b"elsewhere.rs"));
        let empty = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, "").unwrap();
        assert!(!empty.is_all()); assert_eq!(empty.region_files(), Some(0));
        assert!(!empty.includes_path(b"anything.rs"));
        assert!(empty.with_path(AtlasSearchPathKind::Workspace, "").unwrap().is_all());
    }
}
