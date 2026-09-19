#![forbid(unsafe_code)]
//! Explicit cache authority, exact content identity and rebuildable artifacts.
//! A retained OS file lock excludes cooperating writers. Checks reject static
//! symlinks; this does not claim hostile same-user ancestor-race confinement.
//! Sources are still read on every request: metadata does not prove freshness.
//! Artifact publication guarantees atomic visibility, not power-loss durability.
//! Rebuildable entries deliberately avoid per-artifact durable flushes; a lost
//! or corrupt entry after power loss is a miss, never a change to source files.
use std::{collections::VecDeque, fs::{self, File, OpenOptions}, io::{Read, Write},
    path::{Component, Path, PathBuf}, sync::Arc};
use fcb::{ByteLength, document::source_highlight::canonical_language};
use fcb::search::ResourceBudget;
use fcb::store::{EnvelopeLimits, EnvelopeReader, EnvelopeSchema, EnvelopeWriter, Sha256,
    Sha256Digest, UnknownPolicy};
use fcb_core::ResourceLease;
use super::{HostError, HostResponse, MAX_HOST_TEXT_BYTES, source_document};
use crate::{AppError, allocation, owner, output::MAX_RESPONSE_BYTES};

pub const MAX_NATIVE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_DISK_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_RAM_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 4096;
// Bump when lexical behavior, source JSON schema, language selection or offset
// conversion changes. Upstream implementation pinned for this semantic version.
const SOURCE_SEMANTICS: &str = "fcb-source-document/1;utf16/1;fmd-9d4dbe8bee1447113ba8aeadb2eb05d3b48c7032";
const SCHEMA: EnvelopeSchema = EnvelopeSchema::new(0x53434348, 1, 0);
const OVERHEAD: usize = 1024;
const MARKER: &str = "fcb-source-artifacts-v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheError { Root, Busy, Limit, Io, Corrupt, Conflict, Admission, Canceled }
#[derive(Clone, Copy, Debug)]
pub struct CacheLimits { pub ram_bytes: usize, pub disk_bytes: u64, pub entries: usize }
impl Default for CacheLimits {
    fn default() -> Self { Self { ram_bytes: MAX_RAM_BYTES, disk_bytes: MAX_DISK_BYTES, entries: MAX_ENTRIES } }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct CacheStats {
    pub source_bytes_read: u64, pub lexer_calls: u64, pub ram_hits: u64,
    pub disk_hits: u64, pub misses: u64, pub corrupt: u64, pub writes: u64,
    pub write_refusals: u64,
}
struct Hot { domain: &'static str, key: Sha256Digest, bytes: Arc<[u8]> }
pub struct SourceCache {
    root: PathBuf, marker: Vec<u8>, _lock: File, limits: CacheLimits,
    disk_bytes: u64, entries: usize, next_temp: u64,
    hot: VecDeque<Hot>, hot_bytes: usize, stats: CacheStats,
    budget: ResourceBudget, _lease: ResourceLease,
}
impl SourceCache {
    /// Explicitly authorize one private cache directory. Existing non-cache
    /// contents are refused, never adopted or deleted. No threads are started.
    pub fn open(root: &Path, limits: CacheLimits) -> Result<Self, CacheError> {
        if !root.is_absolute() || root.components().count() < 3 || root.as_os_str().len() > 16_384
            || limits.ram_bytes > MAX_RAM_BYTES || limits.disk_bytes > MAX_DISK_BYTES
            || limits.entries == 0 || limits.entries > MAX_ENTRIES { return Err(CacheError::Limit); }
        ensure_directories(root)?;
        require_private(&fs::symlink_metadata(root).map_err(|_| CacheError::Root)?)?;
        let root = fs::canonicalize(root).map_err(|_| CacheError::Root)?;
        let marker = format!("{MARKER}\n{}\n", root.to_str().ok_or(CacheError::Root)?).into_bytes();
        let marker_path = root.join("owner");
        match fs::symlink_metadata(&marker_path) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                if bounded_read(&marker_path, 32 * 1024)? != marker { return Err(CacheError::Root); }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if fs::read_dir(&root).map_err(|_| CacheError::Root)?.next().is_some() { return Err(CacheError::Root); }
                let mut file = private_options().write(true).create_new(true).open(&marker_path).map_err(|_| CacheError::Root)?;
                file.write_all(&marker).map_err(|_| CacheError::Io)?;
                file.sync_all().map_err(|_| CacheError::Io)?;
            }
            _ => return Err(CacheError::Root),
        }
        let lock_path = root.join("lock");
        regular_or_absent(&lock_path)?;
        let lock = private_options().read(true).write(true).create(true).truncate(false).open(lock_path).map_err(|_| CacheError::Root)?;
        lock.try_lock().map_err(|_| CacheError::Busy)?;
        let budget = ResourceBudget::new(owner(), ByteLength::new(512 * 1024 * 1024)).map_err(|_| CacheError::Admission)?;
        // Retained hot bytes plus bounded encoder/read/response overlap. The
        // envelope writer may grow geometrically; reserve that overlap too.
        let charge = limits.ram_bytes + 6 * (MAX_NATIVE_BYTES + OVERHEAD) + MAX_ENTRIES * 256;
        let lease = budget.try_reserve_managed(owner(), allocation(110), ByteLength::new(charge as u64)).map_err(|_| CacheError::Admission)?;
        let mut cache = Self { root, marker, _lock: lock, limits, disk_bytes: 0, entries: 0,
            next_temp: 0, hot: VecDeque::new(), hot_bytes: 0, stats: CacheStats::default(), budget, _lease: lease };
        cache.scan()?;
        Ok(cache)
    }
    pub fn stats(&self) -> CacheStats { self.stats }
    pub fn disk_bytes(&self) -> u64 { self.disk_bytes }
    pub fn ram_bytes(&self) -> usize { self.hot_bytes }
    pub fn root(&self) -> &Path { &self.root }
    pub fn source(&mut self, path: &Path, mut canceled: impl FnMut() -> bool)
        -> Result<(HostResponse, Sha256Digest), HostError> {
        let source = super::read_text(path, MAX_HOST_TEXT_BYTES, &mut canceled)?;
        self.stats.source_bytes_read = self.stats.source_bytes_read.saturating_add(source.source_bytes_read() as u64);
        let language = canonical_language(path.extension().and_then(|s| s.to_str()).unwrap_or(""));
        let mut hash = Sha256::new();
        hash.update(SOURCE_SEMANTICS.as_bytes()); hash.update(&[0]);
        hash.update(language.as_bytes()); hash.update(&[0]);
        hash.update(&(source.as_str().len() as u64).to_le_bytes()); hash.update(source.as_str().as_bytes());
        let key = hash.finalize();
        if canceled() { return Err(AppError::Canceled.into()); }
        if let Ok(Some(bytes)) = self.get("source", key, MAX_RESPONSE_BYTES) {
            if let Ok(text) = std::str::from_utf8(&bytes) {
                if canceled() { return Err(AppError::Canceled.into()); }
                let lease = self.budget.try_reserve_managed(owner(), allocation(111), ByteLength::new((2 * MAX_RESPONSE_BYTES) as u64))
                    .map_err(|_| AppError::Admission)?;
                return Ok((HostResponse { text: text.to_owned(), exit_code: crate::EXIT_OK, _lease: lease }, key));
            }
        }
        self.stats.lexer_calls = self.stats.lexer_calls.saturating_add(1);
        let response = source_document::render(path, &source, &mut canceled)?;
        if canceled() { return Err(AppError::Canceled.into()); }
        if self.put("source", key, response.as_str().as_bytes(), MAX_RESPONSE_BYTES, false).is_err() {
            self.stats.write_refusals = self.stats.write_refusals.saturating_add(1);
        }
        Ok((response, key))
    }
    /// Native callers own platform key completeness and decoded-format checks.
    pub fn get_native(&mut self, key: Sha256Digest) -> Result<Option<Arc<[u8]>>, CacheError> {
        self.get("native", key, MAX_NATIVE_BYTES)
    }
    pub fn put_native(&mut self, key: Sha256Digest, bytes: &[u8]) -> Result<(), CacheError> {
        self.put("native", key, bytes, MAX_NATIVE_BYTES, false)
    }
    /// Explicit replacement after the native consumer rejects a checksummed
    /// artifact's platform semantics. This never replaces a source artifact.
    /// Existing borrowed Arcs keep their original bytes. Failed admission or
    /// publication leaves the incumbent available; no eager deletion occurs.
    pub fn repair_native(&mut self, key: Sha256Digest, bytes: &[u8]) -> Result<(), CacheError> {
        self.put("native", key, bytes, MAX_NATIVE_BYTES, true)
    }
    fn validate(&self) -> Result<(), CacheError> {
        let meta = fs::symlink_metadata(&self.root).map_err(|_| CacheError::Root)?;
        require_private(&meta)?;
        if !meta.is_dir() || meta.file_type().is_symlink()
            || bounded_read(&self.root.join("owner"), 32 * 1024)? != self.marker { return Err(CacheError::Root); }
        Ok(())
    }
    fn scan(&mut self) -> Result<(), CacheError> {
        self.validate()?;
        let (mut count, mut bytes) = (0usize, 0u64);
        for entry in fs::read_dir(&self.root).map_err(|_| CacheError::Root)? {
            let entry = entry.map_err(|_| CacheError::Root)?;
            let name = entry.file_name();
            if name == "owner" || name == "lock" { continue; }
            count = count.checked_add(1).ok_or(CacheError::Limit)?;
            if count > self.limits.entries { return Err(CacheError::Limit); }
            let meta = fs::symlink_metadata(entry.path()).map_err(|_| CacheError::Root)?;
            if !meta.is_file() || meta.file_type().is_symlink() { return Err(CacheError::Root); }
            bytes = bytes.checked_add(meta.len()).ok_or(CacheError::Limit)?;
            if bytes > self.limits.disk_bytes { return Err(CacheError::Limit); }
        }
        self.disk_bytes = bytes; self.entries = count; Ok(())
    }
    fn path(&self, domain: &str, key: Sha256Digest) -> PathBuf { self.root.join(format!("{domain}-{}.bin", key.to_hex())) }
    fn get(&mut self, domain: &'static str, key: Sha256Digest, limit: usize) -> Result<Option<Arc<[u8]>>, CacheError> {
        self.validate()?;
        if let Some(i) = self.hot.iter().position(|h| h.domain == domain && h.key == key) {
            let hit = self.hot.remove(i).ok_or(CacheError::Corrupt)?;
            let bytes = Arc::clone(&hit.bytes); self.hot.push_back(hit);
            self.stats.ram_hits = self.stats.ram_hits.saturating_add(1); return Ok(Some(bytes));
        }
        let path = self.path(domain, key);
        if !path.try_exists().map_err(|_| CacheError::Io)? {
            self.stats.misses = self.stats.misses.saturating_add(1); return Ok(None);
        }
        let loaded = bounded_read(&path, limit + OVERHEAD).and_then(|raw| decode(&raw, domain, key, limit));
        match loaded {
            Ok(bytes) => {
                self.stats.disk_hits = self.stats.disk_hits.saturating_add(1);
                let bytes: Arc<[u8]> = Arc::from(bytes); self.remember(domain, key, Arc::clone(&bytes)); Ok(Some(bytes))
            }
            Err(CacheError::Root) => Err(CacheError::Root),
            Err(_) => { self.stats.corrupt = self.stats.corrupt.saturating_add(1); Ok(None) }
        }
    }
    fn remember(&mut self, domain: &'static str, key: Sha256Digest, bytes: Arc<[u8]>) {
        if bytes.len() > self.limits.ram_bytes { return; }
        while self.hot_bytes > self.limits.ram_bytes - bytes.len() || self.hot.len() >= self.limits.entries {
            if let Some(old) = self.hot.pop_front() { self.hot_bytes -= old.bytes.len(); } else { break; }
        }
        self.hot_bytes += bytes.len(); self.hot.push_back(Hot { domain, key, bytes });
    }
    fn put(&mut self, domain: &'static str, key: Sha256Digest, bytes: &[u8], limit: usize, replace_rejected: bool) -> Result<(), CacheError> {
        if bytes.len() > limit { return Err(CacheError::Limit); }
        self.validate()?;
        if !replace_rejected {
            if let Some(previous) = self.get(domain, key, limit)? {
                return if previous.as_ref() == bytes { Ok(()) } else { Err(CacheError::Conflict) };
            }
        }
        self.scan()?;
        let mut out = EnvelopeWriter::new(SCHEMA);
        out.put_str(domain); out.put_bytes(key.as_bytes()); out.put_bytes(bytes);
        let encoded = out.finish();
        if encoded.len() > limit + OVERHEAD || encoded.len() as u64 > self.limits.disk_bytes.saturating_sub(self.disk_bytes)
            || self.entries >= self.limits.entries { return Err(CacheError::Limit); }
        let destination = self.path(domain, key);
        regular_or_absent(&destination)?;
        self.next_temp = self.next_temp.checked_add(1).ok_or(CacheError::Limit)?;
        let temporary = self.root.join(format!("pending-{}-{}", std::process::id(), self.next_temp));
        let mut file = private_options().write(true).create_new(true).open(&temporary).map_err(|_| CacheError::Io)?;
        // Count before writing so failures cannot silently reset accounting.
        self.entries += 1; self.disk_bytes += encoded.len() as u64;
        file.write_all(&encoded).map_err(|_| CacheError::Io)?;
        // These are disposable derived artifacts, not authoritative user data.
        // Close before rename for atomic reader visibility, without forcing a
        // per-entry disk flush (F_FULLFSYNC on macOS). Power loss can discard or
        // corrupt a cache entry; envelope validation then triggers recomputation.
        drop(file);
        self.validate()?; regular_or_absent(&destination)?;
        fs::rename(&temporary, &destination).map_err(|_| CacheError::Io)?;
        // Publication has succeeded. Retire only this key's hot reference even
        // if the subsequent bookkeeping scan fails; never serve the old value.
        if let Some(index) = self.hot.iter().position(|h| h.domain == domain && h.key == key) {
            if let Some(old) = self.hot.remove(index) { self.hot_bytes -= old.bytes.len(); }
        }
        self.scan()?;
        self.stats.writes = self.stats.writes.saturating_add(1);
        self.remember(domain, key, Arc::from(bytes)); Ok(())
    }
}
fn decode(raw: &[u8], domain: &str, key: Sha256Digest, limit: usize) -> Result<Vec<u8>, CacheError> {
    let mut reader = EnvelopeReader::open(raw, SCHEMA,
        EnvelopeLimits { max_document_bytes: limit + OVERHEAD, max_payload_bytes: limit + OVERHEAD, max_field_bytes: limit.max(32) },
        UnknownPolicy::Strict).map_err(|_| CacheError::Corrupt)?;
    if reader.get_str().map_err(|_| CacheError::Corrupt)? != domain
        || reader.get_bytes().map_err(|_| CacheError::Corrupt)? != key.as_bytes() { return Err(CacheError::Corrupt); }
    let bytes = reader.get_bytes().map_err(|_| CacheError::Corrupt)?.to_vec();
    reader.finish().map_err(|_| CacheError::Corrupt)?; Ok(bytes)
}
fn regular_or_absent(path: &Path) -> Result<(), CacheError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => require_private(&meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(CacheError::Root),
    }
}
fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>, CacheError> {
    regular_or_absent(path)?;
    let file = File::open(path).map_err(|_| CacheError::Io)?;
    if !file.metadata().map_err(|_| CacheError::Io)?.is_file() { return Err(CacheError::Root); }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|_| CacheError::Io)?;
    if bytes.len() > limit { return Err(CacheError::Limit); }
    Ok(bytes)
}
fn ensure_directories(root: &Path) -> Result<(), CacheError> {
    let mut current = PathBuf::new();
    for component in root.components() {
        match component { Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()), _ => return Err(CacheError::Root) }
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
                builder.create(&current).map_err(|_| CacheError::Root)?;
            },
            _ => return Err(CacheError::Root),
        }
    }
    Ok(())
}

// Newly created cache objects are private even under a permissive process
// umask. Existing caller-owned ancestors are neither chmodded nor adopted.
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    options
}
fn require_private(meta: &fs::Metadata) -> Result<(), CacheError> {
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 { return Err(CacheError::Root); }
    }
    #[cfg(not(unix))] let _ = meta;
    Ok(())
}
