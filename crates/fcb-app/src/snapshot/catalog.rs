#![forbid(unsafe_code)]

//! One explicit catalog build route; shared opening for ordinary and indexed
//! snapshot commands. No automatic sidecar discovery, pin inference, or repair.

use std::{fs::{self, File}, io::{self, Read}, path::Path};
use fcb::{ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::search::snapshot::Sha256Digest;
use fcb::search::snapshot_catalog::{CatalogError, PinnedCatalog, MAX_CATALOG_BYTES};
use fcb::search::paged_snapshot::{PagedSnapshot, SnapshotDirectory};
use fcb_core::ResourceLease;
use crate::{allocation, owner, input, EXIT_OK, EXIT_PARTIAL};
use crate::output::{Output, OutputError};
use super::{Settings, Failure, Effect, SnapshotLimits, MAX_SNAPSHOT_BYTES, begin, write_new};

impl From<CatalogError> for Failure {
    fn from(error: CatalogError) -> Self {
        Self { code: error.to_string(), canceled: matches!(error, CatalogError::Canceled
            | CatalogError::Archive(fcb::search::paged_snapshot::PagedSnapshotError::Canceled)) }
    }
}

pub(super) fn parse_pin(text: &str) -> Result<Sha256Digest, Failure> {
    if text.len() != 64 { return Err(Failure::new("CATALOG_INVALID_PIN")); }
    Ok(Sha256Digest::new(super::hex(text, 32)?.try_into().map_err(|_| Failure::new("CATALOG_INVALID_PIN"))?))
}

/// Validates the separately pinned metadata BEFORE touching the archive. The
/// chosen native archive handle is the only source backing; member names never
/// become paths. Missing/untrusted catalogs fail, not silent fallback to scans.
/// allocations = [retained directory, loaded catalog bytes].
pub(super) fn open(path: &Path, catalog_path: Option<&Path>, pin: Option<Sha256Digest>,
    budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
    canceled: &mut impl FnMut() -> bool) -> Result<PagedSnapshot<File>, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    if allocations[0] == allocations[1] { return Err(Failure::new("CATALOG_ALLOCATION_IDS")); }
    let catalog = match (catalog_path, pin) {
        (None, None) => None,
        (Some(path), Some(pin)) => {
            let loaded = load(path, budget, allocations[1], canceled)?;
            Some(PinnedCatalog::decode_pinned(&loaded.bytes, pin, owner(), SnapshotLimits::default(),
                budget, allocations[0], &mut *canceled)?)
        }
        _ => return Err(Failure::new("CATALOG_PATH_AND_PIN_REQUIRED")),
    };
    let source = input::absolute(path)?;
    let (file, metadata) = input::open_regular(&source)?;
    if metadata.len() > MAX_SNAPSHOT_BYTES as u64 { return Err(Failure::new("SNAPSHOT_LIMIT")); }
    match catalog {
        Some(catalog) => Ok(PagedSnapshot::open_pinned(file, catalog, &mut *canceled)?),
        None => Ok(PagedSnapshot::open(file, owner(), SnapshotLimits::default(), budget, allocations[0], &mut *canceled)?),
    }
}

pub(super) fn build(settings: &Settings, out: &mut Output, budget: &ResourceBudget,
    effect: &mut Effect, canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    let source = settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    let destination = input::absolute(settings.output.as_deref().ok_or_else(|| Failure::new("SNAPSHOT_OUTPUT_REQUIRED"))?)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_EXISTS")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_UNAVAILABLE")),
    }
    // Builds ALWAYS revalidate the full archive, never create a new trust receipt
    // from metadata read under another receipt or from boundary checks alone.
    let archive = open(source, None, None, budget, [allocation(104), allocation(110)], canceled)?;
    let directory = archive.directory();
    let artifact = directory.encode_catalog(budget, allocation(111), &mut *canceled)?;
    write_new(&destination, artifact.bytes(), effect, canceled)?;
    if settings.json {
        begin(out, "snapshot-catalog")?;
        out.literal(",\"effect\":")?; out.quoted(effect.name())?;
        out.literal(",\"destination\":")?; out.path(&destination)?;
        out.literal(",\"catalog_digest\":")?; out.quoted(&artifact.digest().to_hex())?;
        out.literal(",\"snapshot_digest\":")?; out.quoted(&directory.digest().to_hex())?;
        out.literal(",\"catalog_bytes\":")?; out.integer(artifact.bytes().len() as u64)?;
        out.literal(",\"archive_validation_bytes\":")?; out.integer(directory.validation_stats().bytes_read)?;
        out.literal(",\"archive_body_verified_on_open\":true,\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":false")?;
        out.literal(",\"discovery_complete\":")?; out.boolean(directory.discovery_complete())?;
        out.literal(",\"known_files\":")?; out.integer(directory.len() as u64)?;
        out.literal(",\"captured_files\":")?; out.integer(directory.captured_files() as u64)?;
        out.literal(",\"source_payload_in_catalog\":false,\"source_derived_sensitive\":true,\"power_loss_qualified\":false}\n")?;
    } else {
        out.literal("Saved verified source catalog. Retain this trusted digest separately:\n")?;
        out.literal(&artifact.digest().to_hex())?; out.literal("\n")?;
        out.literal("Native names and source digests are sensitive metadata. No source payload is included.\n")?;
    }
    Ok(if !directory.discovery_complete() || directory.captured_files() != directory.len() { EXIT_PARTIAL } else { EXIT_OK })
}

pub(super) fn validation_fields(out: &mut Output, directory: &SnapshotDirectory) -> Result<(), OutputError> {
    out.literal(",\"archive_open_validation\":")?;
    out.quoted(if directory.fully_verified_on_open() { "full-archive" } else { "trusted-catalog-and-boundaries" })?;
    out.literal(",\"archive_body_verified_on_open\":")?; out.boolean(directory.fully_verified_on_open())?;
    out.literal(",\"catalog_digest\":")?;
    match directory.catalog_pin() { Some(pin) => out.quoted(&pin.to_hex())?, None => out.literal("null")? }
    out.literal(",\"member_verification\":\"digest-before-publication\"")
}

struct Loaded { bytes: Vec<u8>, _lease: ResourceLease }
fn load(path: &Path, budget: &ResourceBudget, allocation: ResourceAllocationId,
    canceled: &mut impl FnMut() -> bool) -> Result<Loaded, Failure> {
    let path = input::absolute(path)?;
    let (mut file, metadata) = input::open_regular(&path)?;
    let length = usize::try_from(metadata.len()).map_err(|_| Failure::new("CATALOG_LIMIT"))?;
    if length > MAX_CATALOG_BYTES { return Err(Failure::new("CATALOG_LIMIT")); }
    let lease = budget.try_reserve_managed(owner(), allocation, ByteLength::new(length as u64 + 256))
        .map_err(|_| Failure::new("CATALOG_RESOURCE_DENIED"))?;
    let mut bytes = Vec::new(); bytes.try_reserve_exact(length).map_err(|_| Failure::new("CATALOG_RESOURCE_DENIED"))?;
    if bytes.capacity() > length { return Err(Failure::new("CATALOG_RESOURCE_DENIED")); }
    bytes.resize(length, 0);
    let mut offset = 0;
    for _ in 0..131_072 {
        if canceled() { return Err(Failure::canceled()); }
        let end = length.min(offset + 64 * 1024);
        let mut extra = [0u8; 1];
        let target = if offset == length { &mut extra[..] } else { &mut bytes[offset..end] };
        match file.read(target) {
            Ok(0) if offset == length => return Ok(Loaded { bytes, _lease: lease }),
            Ok(n) if offset < length && n > 0 && n <= end - offset => offset += n,
            Ok(_) => return Err(Failure::new("CATALOG_FILE_CHANGED")),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(Failure::new("CATALOG_READ_FAILED")),
        }
    }
    Err(Failure::new("CATALOG_READ_CALL_LIMIT"))
}
