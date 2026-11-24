#![forbid(unsafe_code)]

//! Bounded evidence transfer from a complete retained capture. The receiving
//! desk keeps only an excerpt; original source identities and offsets survive.
//! No live path is opened and no whole-capture transfer is required.

use super::{ByteLength, ByteRange, DetectedEncoding, ExtentViewError, ObservedExtent,
    Path, ReaderSession, ReaderSessionError, check, range};

/// A borrowed source label and an independently admitted immutable excerpt.
/// `HostSupplied` records that these bytes came from the retained capture, not a
/// new live-file observation. The excerpt alone makes no whole-file claim.
pub struct ReaderRetainedPassage<'a> {
    pub(crate) extent: ObservedExtent,
    pub(crate) path: &'a Path,
    pub(crate) encoding: DetectedEncoding,
    pub(crate) selection: ByteRange,
}
impl ReaderRetainedPassage<'_> {
    pub fn extent(&self) -> &ObservedExtent { &self.extent }
    pub fn path(&self) -> &Path { self.path }
    pub const fn encoding(&self) -> DetectedEncoding { self.encoding }
    pub const fn selection(&self) -> ByteRange { self.selection }
}

impl ReaderSession {
    /// Retain at most 256 KiB of selected original bytes plus eight bytes of
    /// decoder context on either side. Capacity is admitted before the bounded
    /// copy. The desk's shared extent decoder subsequently requires exact
    /// scalar/CRLF boundaries; this method never rounds the user's selection.
    pub fn retained_passage(&mut self, selection: ByteRange,
        mut canceled: impl FnMut() -> bool) -> Result<ReaderRetainedPassage<'_>, ReaderSessionError> {
        check(&mut canceled)?;
        if selection.is_empty() || selection.len().get() > fcb::search::extents::MAX_EXTENT_TEXT_BYTES as u64
            || selection.end().get() > self.length() {
            return Err(ReaderSessionError::InvalidRange);
        }
        if !matches!(self.encoding, DetectedEncoding::Utf8 { .. }
            | DetectedEncoding::Utf16Le | DetectedEncoding::Utf16Be) {
            return Err(ExtentViewError::UnsupportedEncoding.into());
        }
        let context = range(selection.start().get().saturating_sub(8),
            selection.end().get().saturating_add(8).min(self.length()))?;
        let request = self.capture.request().with_range(context)?;
        let [allocation] = self.allocations()?;
        let bytes = &self.capture.bytes()[context.start().get() as usize..context.end().get() as usize];
        let extent = ObservedExtent::from_bytes(request, ByteLength::new(self.length()), bytes,
            &self.budget, allocation).map_err(ExtentViewError::from)?;
        check(&mut canceled)?;
        Ok(ReaderRetainedPassage { extent, path: &self.path, encoding: self.encoding, selection })
    }
}
