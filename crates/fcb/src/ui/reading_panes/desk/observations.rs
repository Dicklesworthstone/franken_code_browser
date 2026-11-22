#![forbid(unsafe_code)]

//! Personal passages backed by bounded observed extents. These are neither
//! complete sources nor pane/history locations. Stored paths and origin IDs
//! describe old evidence and never authorize a live path or identity lookup.

use super::*;
use fcb_core::QueryGeneration;
use fcb_source::{CaptureRequest, DetectedEncoding, MappingSpan};
use crate::search::extents::{ExtentError, ExtentText, ExtentViewError, ObservedExtent, MAX_EXTENT_TEXT_BYTES};
#[cfg(feature = "snapshot")]
use crate::store::envelope::{Sha256, Sha256Digest};

pub struct DeskObservation {
    pub(super) id: u64,
    // Process-local dedup authority is never serialized. Restored numeric
    // origin IDs may have been reused by a later host process.
    pub(super) live_import: bool,
    pub(super) origin_owner: ArenaOwnerId,
    pub(super) origin_file: u64,
    pub(super) origin_observation: u64,
    pub(super) path: Vec<u8>,
    pub(super) encoding: DetectedEncoding,
    pub(super) extent: ObservedExtent,
    #[cfg(feature = "snapshot")]
    pub(super) extent_sha256: Sha256Digest,
}
impl DeskObservation {
    pub const fn id(&self) -> u64 { self.id }
    pub const fn origin_owner(&self) -> ArenaOwnerId { self.origin_owner }
    pub const fn origin_file(&self) -> u64 { self.origin_file }
    pub const fn origin_observation(&self) -> u64 { self.origin_observation }
    pub fn path_bytes(&self) -> &[u8] { &self.path }
    pub const fn encoding(&self) -> DetectedEncoding { self.encoding }
    pub fn extent(&self) -> &ObservedExtent { &self.extent }
    #[cfg(feature = "snapshot")]
    pub const fn extent_sha256(&self) -> Sha256Digest { self.extent_sha256 }
}

#[derive(Debug)]
pub struct DeskObservedPassage {
    pub(super) id: u64,
    pub(super) observation: u64,
    pub(super) selection: ByteRange,
    pub(super) label: String,
    pub(super) body: String,
    pub(super) update_sequence: u64,
}
impl DeskObservedPassage {
    pub const fn id(&self) -> u64 { self.id }
    pub const fn observation(&self) -> u64 { self.observation }
    pub const fn selection(&self) -> ByteRange { self.selection }
    pub fn label(&self) -> &str { &self.label }
    pub fn body(&self) -> &str { &self.body }
    pub const fn update_sequence(&self) -> u64 { self.update_sequence }
    pub(super) fn try_copy(&self) -> Result<Self, DeskError> {
        Ok(Self { id: self.id, observation: self.observation, selection: self.selection,
            label: personal_text(&self.label, MAX_BOOKMARK_LABEL_BYTES)?,
            body: personal_text(&self.body, MAX_ANNOTATION_BODY_BYTES)?,
            update_sequence: self.update_sequence })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskObservationChange {
    pub change: DeskChange,
    pub observation: u64,
    pub passage: u64,
    pub next_source_identity: u64,
}

impl ReadingDesk {
    pub fn observations(&self) -> impl ExactSizeIterator<Item = &DeskObservation> {
        self.state.observations.iter().map(Arc::as_ref)
    }
    pub fn observed_passages(&self) -> &[DeskObservedPassage] { &self.state.observed_passages }
    pub fn observation(&self, expected: u64, id: u64) -> Result<&DeskObservation, DeskError> {
        self.validate(expected)?;
        self.state.observations.iter().find(|o| o.id == id).map(Arc::as_ref)
            .ok_or(DeskError::MissingObservation)
    }
    pub fn observed_passage(&self, expected: u64, id: u64) -> Result<&DeskObservedPassage, DeskError> {
        self.validate(expected)?;
        self.state.observed_passages.iter().find(|p| p.id == id).ok_or(DeskError::MissingPassage)
    }

    /// Retain exactly one reviewed selection and its decoder context. Repeated
    /// selections of the same immutable observation share one payload. Origin
    /// identity conflicts compare actual bytes and metadata, not only a digest.
    pub fn import_observation(&mut self, expected: u64, attempt: u64, extent: &ObservedExtent,
        path: &[u8], encoding: DetectedEncoding, selection: ByteRange,
        first_source_identity: u64, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<DeskObservationChange, DeskError> {
        let mut next = self.begin(expected, attempt, &mut canceled)?;
        if first_source_identity == 0 { return Err(DeskError::IdentityExhausted); }
        if path.is_empty() || path.len() > MAX_READING_PATH_BYTES { return Err(DeskError::InvalidLocation); }
        validate_selection_range(extent, selection)?;
        if personal_count(&next) == self.limits.bookmarks { return Err(DeskError::BookmarkLimit); }
        let origin = extent.request();
        let existing = next.observations.iter().find(|o| o.live_import && o.origin_owner == origin.file().owner()
            && o.origin_file == origin.file().get() && o.origin_observation == origin.revision().get());
        let id = if let Some(old) = existing {
            if old.path != path || old.encoding != encoding || !same_evidence(&old.extent, extent) {
                return Err(DeskError::IdentityConflict);
            }
            old.id
        } else {
            if self.retained_source_count() == self.limits.sources { return Err(DeskError::SourceLimit); }
            let bytes = extent.bytes().len() as u64;
            if bytes > self.limits.source_bytes { return Err(DeskError::SourceLimit); }
            if bytes > self.limits.retained_bytes.saturating_sub(self.retained_source_bytes()) {
                return Err(DeskError::RetainedByteLimit);
            }
            let identity = self.next_source_identity()?.max(first_source_identity);
            identity.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
            self.source_high_water = identity;
            let id = next.next_observation;
            next.next_observation = id.checked_add(1).filter(|_| id != 0).ok_or(DeskError::IdentityExhausted)?;
            id
        };
        // Decode in the origin owner's domain while charging the full scratch
        // reservation to this desk. Drop scratch before reserving retained bytes;
        // the same allocation key is never active for both reservations.
        let scratch = self.budget.try_reserve_managed(self.owner, allocation,
            ByteLength::new(decode_charge(selection)?)).map_err(|_| DeskError::ResourceDenied)?;
        validate_selection_with_scratch(extent, encoding, selection, &mut canceled)?;
        drop(scratch);
        if existing.is_none() {
            let identity = self.source_high_water;
            let request = rebound_request(extent.request(), self.owner, identity)?;
            let charge = extent.bytes().len() + path.len() + size_of::<DeskObservation>() + 1024;
            let lease = self.budget.try_reserve_managed(self.owner, allocation, ByteLength::new(charge as u64))
                .map_err(|_| DeskError::ResourceDenied)?;
            let restored = ObservedExtent::restore_recorded(request, extent.range(), extent.observed_length(),
                extent.final_length(), extent.consistency(), extent.bytes(), lease).map_err(extent_error)?;
            let path = owned_path(path)?;
            #[cfg(feature = "snapshot")]
            let extent_sha256 = hash_extent(restored.bytes(), &mut canceled)?;
            next.observations.push(Arc::new(DeskObservation { id, live_import: true, origin_owner: origin.file().owner(),
                origin_file: origin.file().get(), origin_observation: origin.revision().get(), path,
                encoding, extent: restored,
                #[cfg(feature = "snapshot")]
                extent_sha256,
            }));
        }
        let passage = next.next_bookmark;
        next.next_bookmark = passage.checked_add(1).filter(|_| passage != 0).ok_or(DeskError::IdentityExhausted)?;
        next.observed_passages.push(DeskObservedPassage { id: passage, observation: id, selection,
            label: personal_text("Retained passage", MAX_BOOKMARK_LABEL_BYTES)?, body: String::new(), update_sequence: 1 });
        let next_source_identity = self.next_source_identity()?.max(first_source_identity);
        let change = self.publish(next, attempt, None, &mut canceled)?;
        Ok(DeskObservationChange { change, observation: id, passage, next_source_identity })
    }

    /// Decode only the retained selected range. Boundary adjustment is refused:
    /// text and original bytes must still describe exactly the saved selection.
    pub fn read_observed_passage(&self, expected: u64, id: u64, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<ExtentText, DeskError> {
        let passage = self.observed_passage(expected, id)?;
        let observation = self.observation(expected, passage.observation)?;
        let text = decode_selection(&observation.extent, observation.encoding, passage.selection,
            &self.budget, allocation, &mut canceled)?;
        check(&mut canceled)?;
        Ok(text)
    }

    pub fn edit_observed_passage(&mut self, expected: u64, attempt: u64, id: u64,
        expected_update_sequence: u64, label: &str, body: &str,
        mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskError> {
        let mut next = self.begin(expected, attempt, &mut canceled)?;
        let passage = next.observed_passages.iter_mut().find(|p| p.id == id).ok_or(DeskError::MissingPassage)?;
        if passage.update_sequence != expected_update_sequence { return Err(DeskError::StalePassage); }
        validate_personal_text(label, body)?;
        let sequence = passage.update_sequence.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        passage.label = personal_text(label, MAX_BOOKMARK_LABEL_BYTES)?;
        passage.body = personal_text(body, MAX_ANNOTATION_BODY_BYTES)?;
        passage.update_sequence = sequence;
        self.publish(next, attempt, None, &mut canceled)
    }
    pub fn forget_observed_passage(&mut self, expected: u64, attempt: u64, id: u64,
        expected_update_sequence: u64, mut canceled: impl FnMut() -> bool) -> Result<DeskChange, DeskError> {
        let mut next = self.begin(expected, attempt, &mut canceled)?;
        let index = next.observed_passages.iter().position(|p| p.id == id).ok_or(DeskError::MissingPassage)?;
        if next.observed_passages[index].update_sequence != expected_update_sequence { return Err(DeskError::StalePassage); }
        next.observed_passages.remove(index);
        self.publish(next, attempt, None, &mut canceled)
    }
}

fn same_evidence(a: &ObservedExtent, b: &ObservedExtent) -> bool {
    a.request().range() == b.request().range() && a.range() == b.range()
        && a.observed_length() == b.observed_length() && a.final_length() == b.final_length()
        && a.consistency() == b.consistency() && a.bytes() == b.bytes()
}
pub(super) fn rebound_request(original: CaptureRequest, owner: ArenaOwnerId, identity: u64)
    -> Result<CaptureRequest, DeskError> {
    let request = CaptureRequest::new(FileId::new(owner, identity).map_err(|_| DeskError::IdentityExhausted)?,
        SourceRevision::new(owner, identity).map_err(|_| DeskError::IdentityExhausted)?)
        .map_err(|_| DeskError::InvalidLocation)?;
    match original.range() {
        Some(range) => request.with_range(range).map_err(|_| DeskError::InvalidLocation),
        None => Ok(request),
    }
}
pub(super) fn owned_path(path: &[u8]) -> Result<Vec<u8>, DeskError> {
    if path.is_empty() || path.len() > MAX_READING_PATH_BYTES { return Err(DeskError::InvalidLocation); }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(path.len()).map_err(|_| DeskError::ResourceDenied)?;
    if bytes.capacity() > path.len() { return Err(DeskError::ResourceDenied); }
    bytes.extend_from_slice(path);
    Ok(bytes)
}
pub(super) fn validate_selection_range(extent: &ObservedExtent, selection: ByteRange) -> Result<(), DeskError> {
    if selection.is_empty() || selection.len().get() > MAX_EXTENT_TEXT_BYTES as u64 {
        return Err(DeskError::InvalidLocation);
    }
    extent.range_bytes(selection).map_err(extent_error)?;
    Ok(())
}
pub(super) fn decode_charge(selection: ByteRange) -> Result<u64, DeskError> {
    // ExtentView maps at most four preceding bytes and the selected bytes.
    // Include its own +8 slack, every mapping span and its owned descriptor.
    selection.len().get().checked_add(16)
        .and_then(|n| n.checked_mul((4 * size_of::<MappingSpan>() + 16) as u64))
        .and_then(|n| n.checked_add((size_of::<ExtentText>() + 1024) as u64))
        .ok_or(DeskError::ResourceDenied)
}
/// Caller has already admitted `decode_charge(selection)` in its owning budget.
pub(super) fn validate_selection_with_scratch(extent: &ObservedExtent, encoding: DetectedEncoding,
    selection: ByteRange, canceled: &mut impl FnMut() -> bool) -> Result<(), DeskError> {
    let budget = ResourceBudget::new(extent.request().file().owner(), ByteLength::new(decode_charge(selection)?))
        .map_err(|_| DeskError::ResourceDenied)?;
    let allocation = ResourceAllocationId::new(1).map_err(|_| DeskError::ResourceDenied)?;
    let _text = decode_selection(extent, encoding, selection, &budget, allocation, canceled)?;
    Ok(())
}
fn decode_selection(extent: &ObservedExtent, encoding: DetectedEncoding, selection: ByteRange,
    budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
    -> Result<ExtentText, DeskError> {
    validate_selection_range(extent, selection)?;
    let owner = extent.request().file().owner();
    let view = BrowserSession::new(owner).open_extent(extent.clone()).map_err(view_error)?;
    let generation = QueryGeneration::new(owner, 1).map_err(|_| DeskError::InvalidLocation)?;
    let text = view.decode(selection, encoding, generation, budget, allocation, &mut *canceled).map_err(view_error)?;
    if text.range() != selection { return Err(DeskError::SelectionMismatch); }
    let selected = text.text_selection(text.source_to_text(selection).map_err(view_error)?).map_err(view_error)?;
    if selected.original_range != selection || selected.original_bytes != extent.range_bytes(selection).map_err(extent_error)? {
        return Err(DeskError::SelectionMismatch);
    }
    Ok(text)
}
pub(super) fn extent_error(error: ExtentError) -> DeskError {
    match error {
        ExtentError::Canceled => DeskError::Canceled,
        ExtentError::AllocationFailed | ExtentError::ResourceDenied => DeskError::ResourceDenied,
        _ => DeskError::InvalidLocation,
    }
}
fn view_error(error: ExtentViewError) -> DeskError {
    match error {
        ExtentViewError::Canceled => DeskError::Canceled,
        ExtentViewError::ResourceDenied => DeskError::ResourceDenied,
        ExtentViewError::Extent(error) => extent_error(error),
        _ => DeskError::InvalidLocation,
    }
}
#[cfg(feature = "snapshot")]
pub(super) fn hash_extent(bytes: &[u8], canceled: &mut impl FnMut() -> bool) -> Result<Sha256Digest, DeskError> {
    let mut hash = Sha256::new();
    for chunk in bytes.chunks(64 * 1024) { check(canceled)?; hash.update(chunk); }
    check(canceled)?;
    Ok(hash.finalize())
}

#[cfg(test)]
mod tests;
