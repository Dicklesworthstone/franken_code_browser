#![forbid(unsafe_code)]

//! Compatibility tile projection for the existing native shell. Discovery,
//! layout, world-coordinate translation and source capture are the real shared
//! engines. This is bounded one-shot worker preparation, NOT a retained camera
//! callback or a new filesystem/partition/lexer implementation.

use std::{fs, path::Path};
use fcb::{ByteLength, ByteOffset, SourceRevision};
use fcb::map::{LayoutOptions, LayoutRevision, Size2D};
use fcb::map::workspace::{AtlasScope, WorkspaceAtlas, WorkspaceAtlasError, WorkspaceAtlasLimits};
use fcb::search::{CaptureRequest, RawPath, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceError, WorkspaceLimits, WorkspaceStage};
use fcb::source::{CancelFlag, DetectedEncoding, SourceError};
use fcb::source::line_index::{LineNumber, LineWindowScanner, LineWindowStatus};
use crate::{AppError, allocation, file_id, input, owner, workspace};
use crate::output::{Output, OutputError, MAX_ENCODED_BYTES};
use super::{HostError, HostResponse};

const WORLD: f64 = 4096.0;
const MAX_PROFILE_ROWS: usize = 4000;
const ATLAS_MANAGED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyAtlasOptions {
    pub max_files: usize,
    pub max_profile_file_bytes: usize,
    pub max_profile_source_bytes: u64,
    pub max_profile_rows: usize,
}
impl Default for LegacyAtlasOptions {
    fn default() -> Self {
        Self { max_files: 32_768, max_profile_file_bytes: 64 * 1024,
            max_profile_source_bytes: 16 * 1024 * 1024, max_profile_rows: MAX_PROFILE_ROWS }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostAtlasError {
    Host(HostError), Workspace(WorkspaceError), Atlas(WorkspaceAtlasError),
    InvalidLimits, IncompleteDiscovery, NonUtf8Path,
}
impl From<AppError> for HostAtlasError { fn from(e: AppError) -> Self { Self::Host(e.into()) } }
impl From<OutputError> for HostAtlasError { fn from(e: OutputError) -> Self { Self::from(AppError::from(e)) } }
impl From<WorkspaceError> for HostAtlasError { fn from(e: WorkspaceError) -> Self { Self::Workspace(e) } }
impl From<WorkspaceAtlasError> for HostAtlasError { fn from(e: WorkspaceAtlasError) -> Self { Self::Atlas(e) } }
impl std::fmt::Display for HostAtlasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(e) => write!(f, "{e}"), Self::Workspace(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::InvalidLimits => f.write_str("HOST_ATLAS_INVALID_LIMITS"),
            Self::IncompleteDiscovery => f.write_str("HOST_ATLAS_DISCOVERY_INCOMPLETE"),
            Self::NonUtf8Path => f.write_str("HOST_ATLAS_LEGACY_PATH_NOT_UTF8"),
        }
    }
}
impl std::error::Error for HostAtlasError {}

/// Compatibility JSON keeps world/files/path/x/y/w/h/bytes/n/tex. `n` is the
/// number of encoded profile rows, NEVER an unknown or truncated total line
/// count. New fields explicitly describe profile coverage, original identity
/// and source I/O. The legacy path-only format refuses non-UTF-8 names; use
/// host::atlas_plan for reversible native paths and partial discovery instead.
pub fn prepare(root: &Path, options: LegacyAtlasOptions, mut canceled: impl FnMut() -> bool)
    -> Result<HostResponse, HostAtlasError> {
    if options.max_files == 0 || options.max_files > 32_768
        || options.max_profile_file_bytes > 256 * 1024
        || options.max_profile_source_bytes > 64 * 1024 * 1024
        || options.max_profile_rows > MAX_PROFILE_ROWS { return Err(HostAtlasError::InvalidLimits); }
    if canceled() { return Err(AppError::Canceled.into()); }
    let budget = ResourceBudget::new(owner(), ByteLength::new(ATLAS_MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
    // Covers compatibility String + native handoff + source/profile temporary
    // overlap. The catalog, layout, spatial index and encoder lease separately.
    let charge = 3 * MAX_ENCODED_BYTES + 8 * options.max_profile_file_bytes
        + MAX_PROFILE_ROWS * 64 + 256 * 1024;
    let lease = budget.try_reserve_managed(owner(), allocation(94), ByteLength::new(charge as u64))
        .map_err(|_| AppError::Admission)?;
    let root = input::absolute(root)?;
    let meta = fs::symlink_metadata(&root).map_err(|_| AppError::Io)?;
    if meta.file_type().is_symlink() { return Err(AppError::Symlink.into()); }
    if !meta.is_dir() { return Err(AppError::Directory.into()); }
    let root = fs::canonicalize(root).map_err(|_| AppError::Io)?;
    let grant = RootGrant::new(RootId::new(owner(), 1).map_err(|_| AppError::InvalidRange)?, RawPath::from_path(&root));
    let limits = WorkspaceLimits { max_files: options.max_files, max_total_path_bytes: 2 * 1024 * 1024,
        max_file_bytes: 0, max_source_bytes: 0, ..WorkspaceLimits::default() };
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).map_err(AppError::from)?,
        file_id(), limits, false, &budget, allocation(30))?;
    let cancel = CancelFlag::new();
    while catalog.stage() == WorkspaceStage::Discovering {
        if canceled() { return Err(AppError::Canceled.into()); }
        catalog.step(&cancel)?;
    }
    // The old shell cannot represent an unknown membership suffix. Do not hand
    // it a convincing partial tree; the versioned plan route supports that state.
    if !catalog.discovery_complete() { return Err(HostAtlasError::IncompleteDiscovery); }
    for entry in catalog.entries() {
        if canceled() { return Err(AppError::Canceled.into()); }
        std::str::from_utf8(entry.path().as_bytes()).map_err(|_| HostAtlasError::NonUtf8Path)?;
    }
    let atlas = WorkspaceAtlas::build(&catalog, &AtlasScope::All, LayoutRevision::new(owner(), 1).map_err(|_| AppError::InvalidRange)?,
        Size2D::new(WORLD, WORLD).map_err(|_| AppError::InvalidRange)?, LayoutOptions::modest(),
        WorkspaceAtlasLimits::default(), &budget, allocation(60), &mut canceled)?;
    let index = atlas.index(&budget, allocation(61), &mut canceled)?;
    let mut out = Output::new(owner(), MAX_ENCODED_BYTES, &budget, allocation(1))?;
    out.literal("{\"schema\":\"fcb.host-atlas/1\",\"identity_scope\":\"response-local\",\"native_presented\":false,\"discovery_complete\":true,\"policy\":")?;
    out.quoted(catalog.policy_name())?;
    out.literal(",\"metric\":")?; out.quoted(atlas.layout().options().metric().name())?;
    out.literal(",\"profile_semantics\":\"utf8-line-byte-length-neutral-class-not-syntax\",\"profile_retention\":\"summary-only-not-readable-source\",\"world\":{\"w\":4096,\"h\":4096},\"files\":[")?;
    let mut io = workspace::IoCounts::default();
    let mut incomplete = 0usize;
    for (ordinal, entry) in catalog.entries().iter().enumerate() {
        if canceled() { return Err(AppError::Canceled.into()); }
        atlas.validate_active()?;
        let file = catalog.file_id(ordinal).ok_or(AppError::InvalidRange)?;
        let node = atlas.node_for_file(file)?;
        let rect = index.bounds_in(node, index.root_node()).map_err(WorkspaceAtlasError::from)?;
        let request = CaptureRequest::new(file, SourceRevision::new(owner(), ordinal as u64 + 1)
            .map_err(|_| AppError::InvalidRange)?).map_err(|_| AppError::InvalidRange)?;
        let (mut profile, mut captured_bytes) = (None, None);
        let mut state = "disabled";
        if options.max_profile_rows != 0 && options.max_profile_file_bytes != 0 && options.max_profile_source_bytes != 0 {
            if entry.observed_bytes() > options.max_profile_file_bytes as u64 { state = "file-byte-limit"; }
            else if io.bytes >= options.max_profile_source_bytes
                || entry.observed_bytes() > options.max_profile_source_bytes - io.bytes { state = "global-byte-limit"; }
            else {
                match workspace::read_capture(&root, request, entry.path(), options.max_profile_file_bytes,
                    options.max_profile_source_bytes, &mut io, &budget, &mut canceled) {
                    Ok(capture) => match std::str::from_utf8(capture.bytes()) {
                        Ok(text) if !text.contains('\0') => {
                            let prepared = profile_text(text, options.max_profile_rows, &mut canceled)?;
                            state = if prepared.rows() as u64 == prepared.lines { "complete" } else { "row-limit" };
                            captured_bytes = Some(capture.bytes().len() as u64);
                            profile = Some(prepared);
                        }
                        _ => state = "unsupported-text",
                    },
                    Err(SourceError::Canceled) => return Err(AppError::Canceled.into()),
                    Err(_) => state = "unavailable-or-changed",
                }
            }
        }
        if state != "complete" { incomplete += 1; }
        if ordinal > 0 { out.literal(",")?; }
        out.literal("{\"path\":")?;
        out.quoted(std::str::from_utf8(entry.path().as_bytes()).map_err(|_| HostAtlasError::NonUtf8Path)?)?;
        out.literal(",\"path_hex\":")?; out.hex(entry.path().as_bytes())?;
        out.literal(",\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"x\":")?; number(&mut out, rect.min_x())?;
        out.literal(",\"y\":")?; number(&mut out, rect.min_y())?;
        out.literal(",\"w\":")?; number(&mut out, rect.size().width())?;
        out.literal(",\"h\":")?; number(&mut out, rect.size().height())?;
        // Legacy numeric field is display compatibility; full-width consumers
        // use the canonical decimal-string observed_bytes field alongside it.
        out.literal(",\"bytes\":")?; out.literal(&entry.observed_bytes().to_string())?;
        out.literal(",\"observed_bytes\":")?; out.integer(entry.observed_bytes())?;
        out.literal(",\"n\":")?; out.literal(&profile.as_ref().map_or(0, Profile::rows).to_string())?;
        out.literal(",\"tex\":")?; base64(&mut out, profile.as_ref().map_or(&[], |p| p.packed.as_slice()))?;
        out.literal(",\"profile_state\":")?; out.quoted(state)?;
        out.literal(",\"profile_complete\":")?; out.boolean(state == "complete")?;
        out.literal(",\"profile_source_revision\":")?;
        if profile.is_some() { out.integer(request.revision().get())?; } else { out.literal("null")?; }
        out.literal(",\"profile_observed_bytes\":")?;
        if let Some(bytes) = captured_bytes { out.integer(bytes)?; } else { out.literal("null")?; }
        out.literal(",\"source_lines\":")?;
        if let Some(profile) = &profile { out.integer(profile.lines)?; } else { out.literal("null")?; }
        out.literal("}")?;
    }
    out.literal("],\"payload_bytes_read\":")?; out.integer(io.bytes)?;
    out.literal(",\"read_calls\":")?; out.integer(io.calls)?;
    out.literal(",\"incomplete_profiles\":")?; out.integer(incomplete as u64)?;
    out.literal(",\"profile_source_byte_limit\":")?; out.integer(options.max_profile_source_bytes)?;
    out.literal("}\n")?;
    atlas.validate_active()?;
    if canceled() { return Err(AppError::Canceled.into()); }
    let mut text = String::new();
    text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
    if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
    text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
    Ok(HostResponse { text, exit_code: if incomplete == 0 { crate::EXIT_OK } else { crate::EXIT_PARTIAL }, _lease: lease })
}

struct Profile { packed: Vec<u8>, lines: u64 }
impl Profile { fn rows(&self) -> usize { self.packed.len() / 2 } }
fn profile_text(text: &str, row_limit: usize, canceled: &mut impl FnMut() -> bool) -> Result<Profile, HostAtlasError> {
    let bytes = text.as_bytes();
    let mut lengths = Vec::new();
    lengths.try_reserve_exact(row_limit).map_err(|_| AppError::Admission)?;
    if lengths.capacity() > row_limit { return Err(AppError::Admission.into()); }
    let (mut offset, mut lines, mut longest) = (0usize, 0u64, 1usize);
    while offset < bytes.len() {
        if canceled() { return Err(AppError::Canceled.into()); }
        // Reuse the source engine's exact CR/LF/CRLF boundary contract. Its
        // possible bare-CR lookahead is not consumed as part of this row.
        let mut scan = LineWindowScanner::new(LineNumber::new(1).map_err(|_| AppError::InvalidRange)?,
            1, DetectedEncoding::Utf8 { has_bom: false }).map_err(|_| AppError::InvalidRange)?;
        scan.step(ByteOffset::new(0), &bytes[offset..], bytes.len() - offset).map_err(|_| AppError::InvalidRange)?;
        let status = if scan.is_finished() { scan.status() } else { scan.finish().map_err(|_| AppError::InvalidRange)? };
        let LineWindowStatus::Resolved { range, .. } = status else { return Err(AppError::InvalidRange.into()); };
        let end = offset + range.end().get() as usize;
        if end <= offset || end > bytes.len() { return Err(AppError::InvalidRange.into()); }
        let row = &bytes[offset..end];
        let row = row.strip_suffix(b"\r\n").or_else(|| row.strip_suffix(b"\r"))
            .or_else(|| row.strip_suffix(b"\n")).unwrap_or(row);
        // The initial BOM has no visible length, but still belongs to its exact
        // source line. An in-content FEFF is never stripped from another row.
        let row = if offset == 0 { row.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(row) } else { row };
        longest = longest.max(row.len()); lines += 1;
        if lengths.len() < row_limit { lengths.push(row.len()); }
        offset = end;
    }
    let mut packed = Vec::new();
    packed.try_reserve_exact(lengths.len() * 2).map_err(|_| AppError::Admission)?;
    if packed.capacity() > lengths.len() * 2 { return Err(AppError::Admission.into()); }
    for length in lengths {
        packed.push((length as u64 * 255 / longest as u64) as u8);
        packed.push(0); // Density only. The shared upstream lexer owns syntax.
    }
    Ok(Profile { packed, lines })
}
fn number(out: &mut Output, number: f64) -> Result<(), HostAtlasError> {
    if !number.is_finite() { return Err(AppError::InvalidRange.into()); }
    out.literal(&number.to_string())?; Ok(())
}
fn base64(out: &mut Output, bytes: &[u8]) -> Result<(), OutputError> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    out.literal("\"")?;
    for chunk in bytes.chunks(3) {
        let a = chunk[0]; let b = chunk.get(1).copied().unwrap_or(0); let c = chunk.get(2).copied().unwrap_or(0);
        let encoded = [ALPHABET[(a >> 2) as usize], ALPHABET[(((a & 3) << 4) | (b >> 4)) as usize],
            if chunk.len() > 1 { ALPHABET[(((b & 15) << 2) | (c >> 6)) as usize] } else { b'=' },
            if chunk.len() > 2 { ALPHABET[(c & 63) as usize] } else { b'=' }];
        out.literal(std::str::from_utf8(&encoded).expect("base64 alphabet is ASCII"))?;
    }
    out.literal("\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_line_profiles_share_newline_semantics_and_never_claim_syntax() {
        let profile = profile_text("\u{feff}// comment\r\nfn x\r\"x\"\n", 4000, &mut || false).unwrap();
        assert_eq!(profile.lines, 3); assert_eq!(profile.rows(), 3);
        assert_eq!(profile.packed, vec![255, 0, 102, 0, 76, 0]);
        assert_eq!(profile_text("", 4000, &mut || false).unwrap().lines, 0);
        assert_eq!(profile_text("\u{feff}", 4000, &mut || false).unwrap().lines, 1);
        assert_eq!(profile_text("\r\n", 4000, &mut || false).unwrap().lines, 1);
        let limited = profile_text("a\nbb\nccc", 1, &mut || false).unwrap();
        assert_eq!(limited.lines, 3); assert_eq!(limited.rows(), 1); assert_eq!(limited.packed, vec![85, 0]);
    }
}
