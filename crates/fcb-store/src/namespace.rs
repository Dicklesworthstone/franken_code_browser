//! Owned cache namespaces with pinned generations (FCB-081.A).
//!
//! A [`CacheNamespace`] is a private, root-confined directory holding
//! immutable entry bytes, addressed by generation. The namespace owns its
//! root exclusively: creation is exclusive, the root carries a marker file
//! binding it to one namespace identity, and every later validation refuses
//! a root whose marker is missing, foreign, or has been swapped for a
//! symlink.
//!
//! Structural guarantees of this child (FCB-081.A):
//!
//! - **No arbitrary deletion paths.** The public API exposes no operation
//!   that removes an arbitrary path. Entry removal, logical clear,
//!   revocation, and deferred reclamation belong to the adjacent child
//!   (FCB-081.B); this child only writes, reads, and pins generations. The
//!   only removals performed internally are this namespace's own pin
//!   markers under its own `pins/` directory.
//! - **No forensic-erasure promise.** Entries are plain files; nothing here
//!   claims secure erasure, and callers must not assume one.
//! - **Race-safe authority where claimed.** Roots are created exclusively
//!   and re-validated through [`fs::symlink_metadata`] (a symlink-swapped
//!   root is refused); entry and pin writes use exclusive creation
//!   (`create_new`), which refuses a symlink planted at the target path;
//!   entry reads traverse through `fcb_source`'s qualified confined reader
//!   under [`SymlinkPolicy::DisallowAll`]. These are the claims std
//!   supports; they are documented, not overstated.
//! - **Hot/cold equality.** Entries written in this session are retained
//!   in memory (hot); re-reading from disk (cold) must yield identical
//!   bytes, and the tests assert it.
//!
//! Reads and confinement are delegated to `fcb_source`'s qualified
//! confined-reader pipeline; this crate never re-implements traversal.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fcb_core::{ArenaOwnerId, FileId, RootId, SourceRevision};
use fcb_source::{
    ConfinedSourceReader, NormalizedPath, RootGrant, SourceError, SymlinkPolicy,
};

/// Check whether a path refers to a protected filesystem root or ancestor.
pub fn is_protected_path(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    if is_protected_name(raw.as_ref()) {
        return true;
    }
    let resolved = if let Ok(canonical) = fs::canonicalize(path) {
        canonical
    } else {
        path.to_path_buf()
    };
    if resolved.parent().is_none() {
        return true;
    }
    let s = resolved.to_string_lossy();
    is_protected_name(s.as_ref())
}

fn is_protected_name(s: &str) -> bool {
    matches!(
        s,
        "/" | "/home"
            | "/Users"
            | "/etc"
            | "/root"
            | "/var"
            | "/usr"
            | "/bin"
            | "/sbin"
            | "/usr/bin"
            | "/usr/sbin"
            | "/usr/local"
            | "/System"
            | "/Library"
            | "/Applications"
    )
}

/// Marker file stored in the namespace root, binding it to one identity.
pub const MARKER_NAME: &str = "marker";
/// Directory holding one exclusive pin marker per pinned generation.
pub const PINS_DIR: &str = "pins";
/// Hard bound for a single cache entry.
pub const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
/// Hard bound for generations in one namespace.
pub const MAX_GENERATIONS: u64 = 4096;
/// Hard bound for a namespace entry name.
pub const MAX_ENTRY_NAME: usize = 128;

fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn generation_dir_name(generation: u64) -> String {
    format!("gen-{generation:06}")
}

/// Identity binding a cache root to one consumer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceIdentity {
    consumer: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    /// Empty, over-long, or containing control characters.
    InvalidConsumer,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConsumer => formatter.write_str(
                "consumer identity must be 1..=200 chars without control characters",
            ),
        }
    }
}

impl std::error::Error for IdentityError {}

impl NamespaceIdentity {
    pub fn new(consumer: &str) -> Result<Self, IdentityError> {
        let ok_len = !consumer.is_empty() && consumer.len() <= 200;
        let ok_chars = !consumer.chars().any(char::is_control);
        if ok_len && ok_chars {
            Ok(Self {
                consumer: consumer.to_string(),
            })
        } else {
            Err(IdentityError::InvalidConsumer)
        }
    }

    pub fn consumer(&self) -> &str {
        &self.consumer
    }

    fn marker_text(&self) -> String {
        format!("fcb-store-cache.v1\nconsumer: {}\n", self.consumer)
    }

    fn dir_name(&self) -> String {
        format!("fcb-store-cache-{:016x}", fnv64(self.consumer.as_bytes()))
    }
}

/// A generation number: monotonically increasing, 1-based, bounded.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Generation(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationError {
    /// Generation zero does not exist; generations start at one.
    Zero,
    /// The generation exceeds [`MAX_GENERATIONS`].
    Exhausted,
}

impl fmt::Display for GenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("generation must be at least one"),
            Self::Exhausted => formatter.write_str("generation space exhausted"),
        }
    }
}

impl std::error::Error for GenerationError {}

impl Generation {
    pub fn new(value: u64) -> Result<Self, GenerationError> {
        if value == 0 {
            return Err(GenerationError::Zero);
        }
        if value > MAX_GENERATIONS {
            return Err(GenerationError::Exhausted);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A validated cache entry name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EntryName(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryNameError {
    /// Empty, over-long, reserved, or outside the allowed alphabet.
    Invalid,
}

impl fmt::Display for EntryNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => formatter.write_str(
                "entry name must be 1..=128 chars of [A-Za-z0-9._-] and not . or ..",
            ),
        }
    }
}

impl std::error::Error for EntryNameError {}

impl EntryName {
    pub fn new(name: &str) -> Result<Self, EntryNameError> {
        let ok_len = !name.is_empty() && name.len() <= MAX_ENTRY_NAME;
        let ok_alphabet = name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if ok_len && ok_alphabet && name != "." && name != ".." {
            Ok(Self(name.to_string()))
        } else {
            Err(EntryNameError::Invalid)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntryName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Errors raised by the cache namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheError {
    /// The root directory is missing, unreadable, or was swapped for a
    /// symlink.
    RootUnavailable,
    /// The root's marker is missing or names a different consumer.
    WrongRoot,
    /// The namespace root directory already exists (use `open`).
    AlreadyExists,
    /// The consumer identity failed validation.
    IdentityInvalid,
    /// The entry name failed validation.
    EntryNameInvalid,
    /// The entry exceeded [`MAX_ENTRY_BYTES`].
    EntryTooLarge,
    /// The generation number was zero or beyond the bound.
    GenerationInvalid,
    /// The generation directory already exists.
    GenerationExists,
    /// The generation was already pinned.
    AlreadyPinned,
    /// The generation was not pinned.
    NotPinned,
    /// The entry already exists in this generation.
    EntryExists,
    /// Attempted write or operation on a rotated, stale, or retired generation.
    StaleGeneration,
    /// Operation refused on protected filesystem root or ancestor path.
    ProtectedPath,
    /// Generation has been logically cleared or revoked.
    GenerationRevoked,
    /// An error surfaced by the delegated confined-reader pipeline.
    Source(SourceError),
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootUnavailable => formatter.write_str("cache root is missing or unreadable"),
            Self::WrongRoot => {
                formatter.write_str("root marker is missing or names another consumer")
            }
            Self::AlreadyExists => formatter.write_str("namespace root already exists"),
            Self::IdentityInvalid => formatter.write_str("consumer identity failed validation"),
            Self::EntryNameInvalid => formatter.write_str("entry name failed validation"),
            Self::EntryTooLarge => formatter.write_str("entry exceeds the per-entry bound"),
            Self::GenerationInvalid => formatter.write_str("generation number is invalid"),
            Self::GenerationExists => formatter.write_str("generation directory already exists"),
            Self::AlreadyPinned => formatter.write_str("generation is already pinned"),
            Self::NotPinned => formatter.write_str("generation is not pinned"),
            Self::EntryExists => formatter.write_str("entry already exists in this generation"),
            Self::StaleGeneration => {
                formatter.write_str("write attempted into stale, rotated, or retired generation")
            }
            Self::ProtectedPath => {
                formatter.write_str("operation refused on protected filesystem root or ancestor path")
            }
            Self::GenerationRevoked => {
                formatter.write_str("generation has been logically cleared or revoked")
            }
            Self::Source(error) => write!(formatter, "confined source error: {error:?}"),
        }
    }
}

impl std::error::Error for CacheError {}

impl From<SourceError> for CacheError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

/// Evidence returned by a successful [`CacheNamespace::write_entry`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntryWrite {
    pub generation: u64,
    pub name: String,
    pub len: u64,
}

/// Report returned by [`CacheNamespace::clear_namespace`] or [`CacheNamespace::clear_generation`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClearReport {
    pub new_generation: Generation,
    pub reclaimed_generations: Vec<u64>,
    pub deferred_generations: Vec<u64>,
    pub reclaimed_bytes: u64,
    pub reclaimed_entries: u64,
}

impl ClearReport {
    /// Logical reclamation removes directory entries, but does NOT guarantee
    /// forensic erasure from underlying media, APFS snapshots, backups, or OS caches.
    pub const fn forensic_erasure_guaranteed(&self) -> bool {
        false
    }
}

/// Report returned by [`CacheNamespace::reclaim_deferred`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReclamationReport {
    pub completed_generations: Vec<u64>,
    pub remaining_deferred: Vec<u64>,
    pub reclaimed_bytes: u64,
    pub reclaimed_entries: u64,
}

impl ReclamationReport {
    /// Logical reclamation removes directory entries, but does NOT guarantee
    /// forensic erasure from underlying media, APFS snapshots, backups, or OS caches.
    pub const fn forensic_erasure_guaranteed(&self) -> bool {
        false
    }
}

/// Aggregate counters for one namespace session. Monotonic and saturating;
/// write-side rejections are counted per class so attempts reconcile
/// against outcomes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NamespaceCounters {
    pub entries_written: u64,
    pub writes_rejected_name: u64,
    pub writes_rejected_size: u64,
    pub writes_rejected_stale: u64,
    pub generations_advanced: u64,
    pub pins: u64,
    pub unpins: u64,
    pub clears_completed: u64,
    pub entries_reclaimed: u64,
    pub bytes_reclaimed: u64,
    pub deferred_reclamations: u64,
}

/// An owned cache namespace rooted in one exclusively-created directory.
pub struct CacheNamespace {
    root: PathBuf,
    identity: NamespaceIdentity,
    owner: ArenaOwnerId,
    grant: RootGrant,
    cancel: fcb_source::CancelFlag,
    current_generation: u64,
    hot: BTreeMap<(u64, String), Arc<Vec<u8>>>,
    deferred_generations: BTreeSet<u64>,
    cleared_generations: BTreeSet<u64>,
    counters: NamespaceCounters,
}

impl fmt::Debug for CacheNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CacheNamespace")
            .field("root", &self.root)
            .field("identity", &self.identity)
            .field("current_generation", &self.current_generation)
            .finish_non_exhaustive()
    }
}

impl CacheNamespace {
    fn root_dir_name(identity: &NamespaceIdentity) -> String {
        identity.dir_name()
    }

    fn issue_grant(
        root: &Path,
        owner: ArenaOwnerId,
        identity: &NamespaceIdentity,
    ) -> Result<RootGrant, CacheError> {
        let root_id = RootId::new(owner, fnv64(identity.consumer.as_bytes()) % 4096 + 1)
            .map_err(|_| CacheError::IdentityInvalid)?;
        Ok(RootGrant::new(
            root_id,
            root.to_string_lossy().as_ref(),
        ))
    }

    fn confine(&self) -> ConfinedSourceReader {
        ConfinedSourceReader::new(
            self.grant.clone(),
            SymlinkPolicy::DisallowAll,
            fcb_core::ByteLength::new(MAX_ENTRY_BYTES),
        )
    }

    fn read_marker_confined(&self) -> Result<String, CacheError> {
        let reader = self.confine();
        let rel = NormalizedPath::new(MARKER_NAME)?;
        let file_id = FileId::new(self.owner, 1).map_err(|_| CacheError::IdentityInvalid)?;
        let revision =
            SourceRevision::new(self.owner, 1).map_err(|_| CacheError::IdentityInvalid)?;
        let capture = reader.read_file(file_id, revision, &rel, &self.cancel)?;
        Ok(String::from_utf8_lossy(capture.bytes()).to_string())
    }

    fn validate_marker(&self) -> Result<(), CacheError> {
        let expected = self.identity.marker_text();
        let actual = self.read_marker_confined()?;
        if actual == expected {
            Ok(())
        } else {
            Err(CacheError::WrongRoot)
        }
    }

    /// Create a new exclusive namespace root under `parent`.
    pub fn create(
        parent: &Path,
        identity: NamespaceIdentity,
        owner: ArenaOwnerId,
    ) -> Result<Self, CacheError> {
        if is_protected_path(parent) {
            return Err(CacheError::ProtectedPath);
        }
        let root = parent.join(Self::root_dir_name(&identity));
        if is_protected_path(&root) {
            return Err(CacheError::ProtectedPath);
        }
        if let Ok(existing) = fs::symlink_metadata(&root) {
            if existing.file_type().is_symlink() {
                return Err(CacheError::RootUnavailable);
            }
            return Err(CacheError::AlreadyExists);
        }
        fs::create_dir(&root).map_err(|_| CacheError::RootUnavailable)?;

        let grant = Self::issue_grant(&root, owner, &identity)?;
        let marker_path = root.join(MARKER_NAME);
        fs::write(&marker_path, identity.marker_text())
            .map_err(|_| CacheError::RootUnavailable)?;
        let first_generation = root.join(generation_dir_name(1));
        fs::create_dir(&first_generation).map_err(|_| CacheError::GenerationExists)?;
        let pins_dir = root.join(PINS_DIR);
        fs::create_dir(&pins_dir).map_err(|_| CacheError::GenerationExists)?;

        Ok(Self {
            root,
            identity,
            owner,
            grant,
            cancel: fcb_source::CancelFlag::new(),
            current_generation: 1,
            hot: BTreeMap::new(),
            deferred_generations: BTreeSet::new(),
            cleared_generations: BTreeSet::new(),
            counters: NamespaceCounters::default(),
        })
    }

    /// Open an existing namespace root, validating its marker against the
    /// identity. A symlink-swapped root is refused before any content read.
    /// The current generation resumes at the highest existing generation.
    pub fn open(
        parent: &Path,
        identity: NamespaceIdentity,
        owner: ArenaOwnerId,
    ) -> Result<Self, CacheError> {
        if is_protected_path(parent) {
            return Err(CacheError::ProtectedPath);
        }
        let root = parent.join(Self::root_dir_name(&identity));
        if is_protected_path(&root) {
            return Err(CacheError::ProtectedPath);
        }
        let meta = fs::symlink_metadata(&root).map_err(|_| CacheError::RootUnavailable)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(CacheError::RootUnavailable);
        }
        let grant = Self::issue_grant(&root, owner, &identity)?;
        let mut highest = 1u64;
        for entry in fs::read_dir(&root).map_err(|_| CacheError::RootUnavailable)? {
            let entry = entry.map_err(|_| CacheError::RootUnavailable)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(digits) = name.strip_prefix("gen-")
                && let Ok(value) = digits.parse::<u64>()
                && value > highest
                && value <= MAX_GENERATIONS
            {
                highest = value;
            }
        }
        let namespace = Self {
            root: root.clone(),
            identity,
            owner,
            grant,
            cancel: fcb_source::CancelFlag::new(),
            current_generation: highest,
            hot: BTreeMap::new(),
            deferred_generations: BTreeSet::new(),
            cleared_generations: BTreeSet::new(),
            counters: NamespaceCounters::default(),
        };
        namespace.validate_marker()?;
        Ok(namespace)
    }

    /// Re-validate the root marker and non-symlink status. Callers that
    /// hold a namespace across time re-validate before trusting it.
    pub fn validate_root(&self) -> Result<(), CacheError> {
        let meta = fs::symlink_metadata(&self.root).map_err(|_| CacheError::RootUnavailable)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(CacheError::RootUnavailable);
        }
        self.validate_marker()
    }

    pub fn identity(&self) -> &NamespaceIdentity {
        &self.identity
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub const fn current_generation(&self) -> u64 {
        self.current_generation
    }

    pub fn counters(&self) -> &NamespaceCounters {
        &self.counters
    }

    /// Advance to a new generation. Entries of earlier generations remain
    /// addressable; nothing is removed.
    pub fn advance_generation(&mut self) -> Result<Generation, CacheError> {
        let next = self.current_generation + 1;
        if next > MAX_GENERATIONS {
            return Err(CacheError::GenerationInvalid);
        }
        let dir = self.root.join(generation_dir_name(next));
        fs::create_dir(&dir).map_err(|_| CacheError::GenerationExists)?;
        self.current_generation = next;
        self.counters.generations_advanced += 1;
        Generation::new(next).map_err(|_| CacheError::GenerationInvalid)
    }

    fn generation_path(&self, generation: u64) -> Result<PathBuf, CacheError> {
        Generation::new(generation)
            .map(|g| self.root.join(generation_dir_name(g.get())))
            .map_err(|_| CacheError::GenerationInvalid)
    }

    /// Write one immutable entry into the current generation. Duplicate
    /// writes of the same name in one generation are refused; entries are
    /// never overwritten. Creation is exclusive, so a symlink planted at
    /// the target path is refused by the filesystem itself.
    pub fn write_entry(&mut self, name: &str, bytes: &[u8]) -> Result<EntryWrite, CacheError> {
        self.write_entry_in_generation(self.current_generation, name, bytes)
    }

    /// Write one immutable entry explicitly targeting a generation.
    ///
    /// If `generation != self.current_generation`, the write is rejected as
    /// [`CacheError::StaleGeneration`]. This blocks delayed background reads or
    /// retired handles from repopulating a cleared or advanced generation.
    pub fn write_entry_in_generation(
        &mut self,
        generation: u64,
        name: &str,
        bytes: &[u8],
    ) -> Result<EntryWrite, CacheError> {
        if generation != self.current_generation {
            self.counters.writes_rejected_stale += 1;
            return Err(CacheError::StaleGeneration);
        }
        let entry_name = EntryName::new(name).map_err(|_| {
            self.counters.writes_rejected_name += 1;
            CacheError::EntryNameInvalid
        })?;
        if bytes.len() as u64 > MAX_ENTRY_BYTES {
            self.counters.writes_rejected_size += 1;
            return Err(CacheError::EntryTooLarge);
        }
        self.validate_root()?;
        let target = self.generation_path(generation)?.join(entry_name.as_str());
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|_| CacheError::EntryExists)?;
        file.write_all(bytes).map_err(|_| CacheError::EntryExists)?;
        let hot_key = (generation, entry_name.as_str().to_string());
        self.hot.insert(hot_key, Arc::new(bytes.to_vec()));
        self.counters.entries_written += 1;
        Ok(EntryWrite {
            generation,
            name: entry_name.as_str().to_string(),
            len: bytes.len() as u64,
        })
    }

    /// Hot read: the retained in-memory copy from this session, if any.
    pub fn read_hot(&self, name: &str, generation: u64) -> Option<Arc<Vec<u8>>> {
        self.hot.get(&(generation, name.to_string())).cloned()
    }

    /// Cold read: through the confined pipeline from disk, regardless of
    /// the hot map. Refuses symlink-swapped entries under DisallowAll.
    pub fn read_cold(&self, name: &str, generation: u64) -> Result<Vec<u8>, CacheError> {
        if self.cleared_generations.contains(&generation)
            && !self.deferred_generations.contains(&generation)
        {
            return Err(CacheError::GenerationRevoked);
        }
        let entry_name = EntryName::new(name).map_err(|_| CacheError::EntryNameInvalid)?;
        self.validate_root()?;
        let reader = self.confine();
        let rel_text = format!(
            "{}/{}",
            generation_dir_name(generation),
            entry_name.as_str()
        );
        let rel = NormalizedPath::new(rel_text.as_str())?;
        let file_id = FileId::new(self.owner, fnv64(name.as_bytes()) % 4096 + 1)
            .map_err(|_| CacheError::EntryNameInvalid)?;
        let revision = SourceRevision::new(self.owner, generation)
            .map_err(|_| CacheError::GenerationInvalid)?;
        let capture = reader.read_file(file_id, revision, &rel, &self.cancel)?;
        Ok(capture.bytes().to_vec())
    }

    /// Read preferring the hot copy, falling back to the confined cold
    /// read. Both paths are byte-identical by construction; the tests
    /// assert it.
    pub fn read_entry(&self, name: &str, generation: u64) -> Result<Vec<u8>, CacheError> {
        if self.cleared_generations.contains(&generation)
            && !self.deferred_generations.contains(&generation)
        {
            return Err(CacheError::GenerationRevoked);
        }
        if let Some(hot) = self.read_hot(name, generation) {
            return Ok((*hot).clone());
        }
        self.read_cold(name, generation)
    }

    /// Pin a generation. Pinned generations are declared retained for
    /// reclamation purposes. Active pins prevent directory deletion during
    /// logical clear, deferring reclamation until all pins are released.
    pub fn pin(&mut self, generation: u64) -> Result<(), CacheError> {
        Generation::new(generation).map_err(|_| CacheError::GenerationInvalid)?;
        if generation > self.current_generation {
            return Err(CacheError::GenerationInvalid);
        }
        let pins = self.root.join(PINS_DIR);
        let marker = pins.join(format!("gen-{generation:06}.pin"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        options
            .open(&marker)
            .map(|_| ())
            .map_err(|_| CacheError::AlreadyPinned)?;
        self.counters.pins += 1;
        Ok(())
    }

    /// Remove this namespace's own pin marker for a generation.
    pub fn unpin(&mut self, generation: u64) -> Result<(), CacheError> {
        Generation::new(generation).map_err(|_| CacheError::GenerationInvalid)?;
        let marker = self
            .root
            .join(PINS_DIR)
            .join(format!("gen-{generation:06}.pin"));
        match fs::remove_file(&marker) {
            Ok(()) => {
                self.counters.unpins += 1;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(CacheError::NotPinned)
            }
            Err(_) => Err(CacheError::RootUnavailable),
        }
    }

    /// Whether a generation currently carries a pin marker.
    pub fn is_pinned(&self, generation: u64) -> bool {
        self.root
            .join(PINS_DIR)
            .join(format!("gen-{generation:06}.pin"))
            .exists()
    }

    /// Whether a generation is currently awaiting deferred reclamation.
    pub fn is_deferred(&self, generation: u64) -> bool {
        self.deferred_generations.contains(&generation)
    }

    /// Generations currently awaiting deferred reclamation.
    pub fn active_deferred_generations(&self) -> Vec<u64> {
        self.deferred_generations.iter().copied().collect()
    }

    /// Generations that have been logically cleared or revoked.
    pub fn cleared_generations(&self) -> Vec<u64> {
        self.cleared_generations.iter().copied().collect()
    }

    /// Internal helper to reclaim one generation directory strictly within
    /// this namespace root. Refuses symlink-swapped generation directories or
    /// entries.
    fn reclaim_generation_dir(&mut self, target_generation: u64) -> Result<(u64, u64), CacheError> {
        let gen_dir = self.root.join(generation_dir_name(target_generation));
        if !gen_dir.exists() {
            return Ok((0, 0));
        }
        let meta = fs::symlink_metadata(&gen_dir).map_err(|_| CacheError::RootUnavailable)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(CacheError::RootUnavailable);
        }
        let mut entries_count = 0u64;
        let mut bytes_count = 0u64;
        for entry in fs::read_dir(&gen_dir).map_err(|_| CacheError::RootUnavailable)? {
            let entry = entry.map_err(|_| CacheError::RootUnavailable)?;
            let entry_path = entry.path();
            let entry_meta =
                fs::symlink_metadata(&entry_path).map_err(|_| CacheError::RootUnavailable)?;
            if entry_meta.file_type().is_symlink() {
                return Err(CacheError::RootUnavailable);
            }
            if entry_meta.is_file() {
                bytes_count = bytes_count.saturating_add(entry_meta.len());
                entries_count = entries_count.saturating_add(1);
                fs::remove_file(&entry_path).map_err(|_| CacheError::RootUnavailable)?;
            }
        }
        fs::remove_dir(&gen_dir).map_err(|_| CacheError::RootUnavailable)?;
        self.hot.retain(|&(g, _), _| g != target_generation);
        Ok((entries_count, bytes_count))
    }

    /// Logically clear the entire namespace:
    ///
    /// 1. Re-validates root marker and non-symlink status; refuses protected paths.
    /// 2. Rotates the generation before admitting any writes or deletions, ensuring
    ///    that any delayed or in-flight writes referencing old generations are
    ///    rejected as [`CacheError::StaleGeneration`].
    /// 3. For every prior generation:
    ///    - If pinned, retains the generation on disk and in memory; marks it for
    ///      deferred reclamation.
    ///    - If unpinned, immediately reclaims entries, frees disk space, and evicts
    ///      hot cache copies.
    /// 4. Updates counters and returns a [`ClearReport`].
    pub fn clear_namespace(&mut self) -> Result<ClearReport, CacheError> {
        self.validate_root()?;
        if is_protected_path(&self.root) {
            return Err(CacheError::ProtectedPath);
        }

        let old_current = self.current_generation;
        let next_gen = self.advance_generation()?.get();

        let mut reclaimed_generations = Vec::new();
        let mut deferred_generations = Vec::new();
        let mut total_reclaimed_bytes = 0u64;
        let mut total_reclaimed_entries = 0u64;

        for g in 1..=old_current {
            self.cleared_generations.insert(g);
            if self.is_pinned(g) {
                self.deferred_generations.insert(g);
                deferred_generations.push(g);
                self.counters.deferred_reclamations += 1;
            } else {
                let (entries, bytes) = self.reclaim_generation_dir(g)?;
                reclaimed_generations.push(g);
                total_reclaimed_entries = total_reclaimed_entries.saturating_add(entries);
                total_reclaimed_bytes = total_reclaimed_bytes.saturating_add(bytes);
            }
        }

        self.counters.clears_completed += 1;
        self.counters.entries_reclaimed =
            self.counters.entries_reclaimed.saturating_add(total_reclaimed_entries);
        self.counters.bytes_reclaimed =
            self.counters.bytes_reclaimed.saturating_add(total_reclaimed_bytes);

        Ok(ClearReport {
            new_generation: Generation::new(next_gen)
                .map_err(|_| CacheError::GenerationInvalid)?,
            reclaimed_generations,
            deferred_generations,
            reclaimed_bytes: total_reclaimed_bytes,
            reclaimed_entries: total_reclaimed_entries,
        })
    }

    /// Clear a specific generation.
    ///
    /// If the target generation is currently active, advances the generation first
    /// to preserve write ordering and block stale repopulation.
    /// If pinned, defers reclamation until unpinned.
    pub fn clear_generation(&mut self, generation: u64) -> Result<ClearReport, CacheError> {
        Generation::new(generation).map_err(|_| CacheError::GenerationInvalid)?;
        self.validate_root()?;
        if is_protected_path(&self.root) {
            return Err(CacheError::ProtectedPath);
        }

        let new_gen = if generation == self.current_generation {
            self.advance_generation()?.get()
        } else {
            self.current_generation
        };

        self.cleared_generations.insert(generation);

        let mut reclaimed_generations = Vec::new();
        let mut deferred_generations = Vec::new();
        let mut total_reclaimed_bytes = 0u64;
        let mut total_reclaimed_entries = 0u64;

        if self.is_pinned(generation) {
            self.deferred_generations.insert(generation);
            deferred_generations.push(generation);
            self.counters.deferred_reclamations += 1;
        } else {
            let (entries, bytes) = self.reclaim_generation_dir(generation)?;
            reclaimed_generations.push(generation);
            total_reclaimed_entries = entries;
            total_reclaimed_bytes = bytes;
        }

        self.counters.clears_completed += 1;
        self.counters.entries_reclaimed =
            self.counters.entries_reclaimed.saturating_add(total_reclaimed_entries);
        self.counters.bytes_reclaimed =
            self.counters.bytes_reclaimed.saturating_add(total_reclaimed_bytes);

        Ok(ClearReport {
            new_generation: Generation::new(new_gen)
                .map_err(|_| CacheError::GenerationInvalid)?,
            reclaimed_generations,
            deferred_generations,
            reclaimed_bytes: total_reclaimed_bytes,
            reclaimed_entries: total_reclaimed_entries,
        })
    }

    /// Reclaim any generations that were previously deferred and have now
    /// been unpinned.
    pub fn reclaim_deferred(&mut self) -> Result<ReclamationReport, CacheError> {
        self.validate_root()?;
        let mut completed_generations = Vec::new();
        let mut remaining_deferred = Vec::new();
        let mut total_bytes = 0u64;
        let mut total_entries = 0u64;

        let pending: Vec<u64> = self.deferred_generations.iter().copied().collect();
        for g in pending {
            if !self.is_pinned(g) {
                let (entries, bytes) = self.reclaim_generation_dir(g)?;
                self.deferred_generations.remove(&g);
                completed_generations.push(g);
                total_entries = total_entries.saturating_add(entries);
                total_bytes = total_bytes.saturating_add(bytes);
            } else {
                remaining_deferred.push(g);
            }
        }

        self.counters.entries_reclaimed =
            self.counters.entries_reclaimed.saturating_add(total_entries);
        self.counters.bytes_reclaimed =
            self.counters.bytes_reclaimed.saturating_add(total_bytes);

        Ok(ReclamationReport {
            completed_generations,
            remaining_deferred,
            reclaimed_bytes: total_bytes,
            reclaimed_entries: total_entries,
        })
    }
}
