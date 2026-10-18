#![forbid(unsafe_code)]

//! Frozen query admission, independent of the atlas's display-only filter.
//! Match native path bytes so undecodable parent/basename bytes do not hide an
//! otherwise valid ASCII extension. No source is opened while selecting files.

use super::*;

pub const MAX_SEARCH_SCOPE_TOKENS: usize = 32;
pub const MAX_SEARCH_SCOPE_TOKEN_BYTES: usize = 64;
pub const MAX_SEARCH_SCOPE_LIST_BYTES: usize = 1024;

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
/// a file outside the atlas's original frozen catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasSearchScope {
    all: bool,
    extensions: Tokens,
    filenames: Tokens,
}
impl Default for AtlasSearchScope { fn default() -> Self { Self::all() } }
impl AtlasSearchScope {
    pub const fn all() -> Self { Self { all: true, extensions: Tokens::EMPTY, filenames: Tokens::EMPTY } }
    pub const fn is_all(&self) -> bool { self.all }
    pub fn from_csv(extensions: Option<&str>, filenames: Option<&str>) -> Result<Self, AtlasSearchError> {
        if extensions.is_none() && filenames.is_none() { return Ok(Self::all()); }
        Ok(Self { all: false, extensions: Tokens::parse(extensions.unwrap_or(""), false)?,
            filenames: Tokens::parse(filenames.unwrap_or(""), true)? })
    }
    pub fn includes_path(&self, path: &[u8]) -> bool {
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
        Ok(())
    }
    pub(super) fn eligible_count(&self, atlas: &AtlasSession, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, AtlasSearchError> {
        if self.all { return Ok(atlas.atlas().file_count()); }
        let mut count = 0;
        for entry in atlas.atlas().catalog().entries() {
            check(canceled)?;
            if self.includes_path(entry.path().raw().as_bytes()) { count += 1; }
        }
        Ok(count)
    }
    /// Caller reserves the exact selected-ID capacity before this allocation.
    /// All retains the legacy catalog cursor without a duplicate universe vector.
    pub(super) fn freeze(&self, atlas: &AtlasSession, eligible: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<Vec<FileId>>, AtlasSearchError> {
        if self.all { return Ok(None); }
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
