#![forbid(unsafe_code)]

//! Immutable source chunks and observed-snapshot contract (FCB-011.A / fcb-8t9.1).
//!
//! Source storage owns immutable byte chunks and a checked `u64` logical length.
//! Text decoding, line boundaries, token spans, and visual glyph runs are derived
//! layers. Original bytes are never reconstructed from rendered glyphs.
//!
//! For ordinary files, an owned contiguous snapshot ([`crate::CompleteCapture`]) is
//! efficient. For large files, bounded chunks ([`ChunkedCapture`]) allow streaming,
//! caching, and sparse access without requiring multi-gigabyte contiguous allocations.
//! Chunk size is selected empirically from a candidate set rather than hard-coded.
//!
//! Range reads return exact bytes plus revision, or a typed error. Mutable source
//! files are NEVER memory-mapped; owned read buffers with before/after stat detection
//! prevent external truncation or concurrent replacement from causing memory faults.
//! Evicted old captures never silently re-read live mutable source.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};

use crate::{CancelFlag, CaptureRequest, ObservationDigest, SourceError};

/// Permitted chunk size selected from a bounded candidate set per FCB-011 §10.1.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChunkSize(usize);

impl ChunkSize {
    /// 16 KiB chunk candidate.
    pub const KB_16: Self = Self(16 * 1024);
    /// 64 KiB chunk candidate (default).
    pub const KB_64: Self = Self(64 * 1024);
    /// 256 KiB chunk candidate.
    pub const KB_256: Self = Self(256 * 1024);

    /// Standard production candidate set for empirical selection.
    pub const CANDIDATE_SIZES: [Self; 3] = [Self::KB_16, Self::KB_64, Self::KB_256];

    /// Construct from one of the standard candidate sizes.
    pub fn from_candidate(bytes: usize) -> Result<Self, SourceError> {
        for candidate in Self::CANDIDATE_SIZES {
            if candidate.0 == bytes {
                return Ok(candidate);
            }
        }
        Err(SourceError::MetadataMismatch)
    }

    /// Construct a chunk size bounded between 1 byte and 1 MiB.
    /// Allows empirical exploration or test fixtures while preventing pathological sizes.
    pub fn bounded(bytes: usize) -> Result<Self, SourceError> {
        if (1..=1024 * 1024).contains(&bytes) {
            Ok(Self(bytes))
        } else {
            Err(SourceError::MetadataMismatch)
        }
    }

    pub const fn bytes(self) -> usize {
        self.0
    }

    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }
}

impl Default for ChunkSize {
    fn default() -> Self {
        Self::KB_64
    }
}

/// One immutable byte chunk of a source capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceChunk {
    index: u32,
    range: ByteRange,
    bytes: Arc<[u8]>,
    digest: ObservationDigest,
}

impl SourceChunk {
    /// Create a validated source chunk.
    ///
    /// Refuses metadata mismatch if the range length does not exactly equal the byte length.
    pub fn new(index: u32, range: ByteRange, bytes: Arc<[u8]>) -> Result<Self, SourceError> {
        if range.len().get() != bytes.len() as u64 {
            return Err(SourceError::MetadataMismatch);
        }
        let digest = ObservationDigest::observe(&bytes);
        Ok(Self {
            index,
            range,
            bytes,
            digest,
        })
    }

    pub const fn index(&self) -> u32 {
        self.index
    }

    pub const fn range(&self) -> ByteRange {
        self.range
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn arc_bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    pub const fn digest(&self) -> ObservationDigest {
        self.digest
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Slice a sub-range within this chunk.
    pub fn slice(&self, sub_range: ByteRange) -> Result<&[u8], SourceError> {
        if sub_range.is_empty() {
            return Err(SourceError::InvalidRange);
        }
        if sub_range.start() < self.range.start() || sub_range.end() > self.range.end() {
            return Err(SourceError::RangeOutOfBounds);
        }
        let rel_start = (sub_range.start().get() - self.range.start().get()) as usize;
        let rel_end = (sub_range.end().get() - self.range.start().get()) as usize;
        Ok(&self.bytes[rel_start..rel_end])
    }
}

/// The exact byte payload returned by a range read, paired with its source revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactRangeResult {
    range: ByteRange,
    revision: SourceRevision,
    bytes: Arc<[u8]>,
    contiguous: bool,
}

impl ExactRangeResult {
    pub fn new(
        range: ByteRange,
        revision: SourceRevision,
        bytes: Arc<[u8]>,
        contiguous: bool,
    ) -> Result<Self, SourceError> {
        if range.len().get() != bytes.len() as u64 {
            return Err(SourceError::MetadataMismatch);
        }
        Ok(Self {
            range,
            revision,
            bytes,
            contiguous,
        })
    }

    pub const fn range(&self) -> ByteRange {
        self.range
    }

    pub const fn revision(&self) -> SourceRevision {
        self.revision
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn arc_bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Whether the exact range was served directly from a single contiguous chunk.
    pub const fn is_contiguous(&self) -> bool {
        self.contiguous
    }
}

/// An immutable owned capture partitioned into bounded chunks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkedCapture {
    request: CaptureRequest,
    total_length: ByteLength,
    chunk_size: ChunkSize,
    chunks: Vec<SourceChunk>,
    digest: ObservationDigest,
}

impl ChunkedCapture {
    /// Construct and validate a chunked capture.
    ///
    /// Validates:
    /// 1. If length is 0, chunk list must be empty.
    /// 2. If length > 0, chunk list must cover [0, total_length) with no gaps and no overlaps.
    /// 3. All non-terminal chunks must have length exactly equal to `chunk_size`.
    /// 4. The terminal chunk must have length equal to `total_length % chunk_size` (or `chunk_size` if evenly divisible).
    /// 5. Chunk indexes are strictly sequential (0, 1, ...).
    pub fn new(
        request: CaptureRequest,
        total_length: ByteLength,
        chunk_size: ChunkSize,
        chunks: Vec<SourceChunk>,
    ) -> Result<Self, SourceError> {
        let total_u64 = total_length.get();
        if total_u64 == 0 {
            if !chunks.is_empty() {
                return Err(SourceError::MetadataMismatch);
            }
            return Ok(Self {
                request,
                total_length,
                chunk_size,
                chunks,
                digest: ObservationDigest::observe(&[]),
            });
        }

        if chunks.is_empty() {
            return Err(SourceError::MetadataMismatch);
        }

        let chunk_bytes_u64 = chunk_size.as_u64();
        let expected_count = total_u64.div_ceil(chunk_bytes_u64);
        if chunks.len() as u64 != expected_count {
            return Err(SourceError::MetadataMismatch);
        }

        let mut expected_start: u64 = 0;
        let mut total_observed: u64 = 0;

        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.index() != i as u32 {
                return Err(SourceError::MetadataMismatch);
            }
            if chunk.range().start().get() != expected_start {
                return Err(SourceError::MetadataMismatch);
            }

            let is_last = i + 1 == chunks.len();
            let expected_chunk_len = if is_last {
                total_u64 - expected_start
            } else {
                chunk_bytes_u64
            };

            if chunk.range().len().get() != expected_chunk_len {
                return Err(SourceError::MetadataMismatch);
            }
            if chunk.len() as u64 != expected_chunk_len {
                return Err(SourceError::MetadataMismatch);
            }

            expected_start = expected_start
                .checked_add(expected_chunk_len)
                .ok_or(SourceError::RangeOutOfBounds)?;
            total_observed = total_observed
                .checked_add(expected_chunk_len)
                .ok_or(SourceError::RangeOutOfBounds)?;
        }

        if total_observed != total_u64 {
            return Err(SourceError::MetadataMismatch);
        }

        let digest = Self::calculate_composite_digest(&chunks);

        Ok(Self {
            request,
            total_length,
            chunk_size,
            chunks,
            digest,
        })
    }

    /// Calculate deterministic digest across all chunk byte contents.
    /// Matches `ObservationDigest::observe` computed over the sequential concatenation of chunks.
    fn calculate_composite_digest(chunks: &[SourceChunk]) -> ObservationDigest {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut total_bytes: u64 = 0;
        for chunk in chunks {
            for byte in chunk.bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            total_bytes += chunk.bytes().len() as u64;
        }
        hash ^= total_bytes.rotate_left(32);
        ObservationDigest::from_raw(hash)
    }

    pub const fn request(&self) -> &CaptureRequest {
        &self.request
    }

    pub const fn total_length(&self) -> ByteLength {
        self.total_length
    }

    pub const fn chunk_size(&self) -> ChunkSize {
        self.chunk_size
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn chunks(&self) -> &[SourceChunk] {
        &self.chunks
    }

    pub fn chunk(&self, index: usize) -> Option<&SourceChunk> {
        self.chunks.get(index)
    }

    pub const fn digest(&self) -> ObservationDigest {
        self.digest
    }

    /// Read an exact byte range from the chunked capture.
    ///
    /// If the range falls entirely within a single chunk, extracts the slice with zero or
    /// single allocation. If the range crosses chunk boundaries, stitches the exact bytes
    /// into a newly allocated buffer sized precisely to the requested range length.
    pub fn range_read(&self, range: ByteRange) -> Result<ExactRangeResult, SourceError> {
        if range.is_empty() {
            return Err(SourceError::InvalidRange);
        }

        let start = range.start().get();
        let end = range.end().get();
        let total = self.total_length.get();

        if end > total {
            return Err(SourceError::RangeOutOfBounds);
        }

        let chunk_bytes = self.chunk_size.as_u64();
        let start_chunk_idx = (start / chunk_bytes) as usize;
        let end_chunk_idx = ((end - 1) / chunk_bytes) as usize;

        if start_chunk_idx >= self.chunks.len() || end_chunk_idx >= self.chunks.len() {
            return Err(SourceError::ChunkOutOfBounds);
        }

        if start_chunk_idx == end_chunk_idx {
            let chunk = &self.chunks[start_chunk_idx];
            let rel_start = (start - chunk.range().start().get()) as usize;
            let rel_end = (end - chunk.range().start().get()) as usize;
            let sub = &chunk.bytes()[rel_start..rel_end];

            let bytes: Arc<[u8]> = if rel_start == 0 && rel_end == chunk.len() {
                Arc::clone(chunk.arc_bytes())
            } else {
                Arc::from(sub)
            };

            return Ok(ExactRangeResult {
                range,
                revision: self.request.revision(),
                bytes,
                contiguous: true,
            });
        }

        // Cross-chunk range read
        let total_req_len = usize::try_from(end - start).map_err(|_| SourceError::PayloadTooLarge)?;
        let mut buffer = Vec::with_capacity(total_req_len);

        for chunk in &self.chunks[start_chunk_idx..=end_chunk_idx] {
            let chunk_start = chunk.range().start().get();
            let chunk_end = chunk.range().end().get();

            let overlap_start = start.max(chunk_start);
            let overlap_end = end.min(chunk_end);

            if overlap_start < overlap_end {
                let rel_start = (overlap_start - chunk_start) as usize;
                let rel_end = (overlap_end - chunk_start) as usize;
                buffer.extend_from_slice(&chunk.bytes()[rel_start..rel_end]);
            }
        }

        if buffer.len() != total_req_len {
            return Err(SourceError::MetadataMismatch);
        }

        Ok(ExactRangeResult {
            range,
            revision: self.request.revision(),
            bytes: Arc::from(buffer.into_boxed_slice()),
            contiguous: false,
        })
    }
}

/// Configuration for reading files using chunked owned buffers.
#[derive(Clone, Copy, Debug)]
pub struct ChunkedReaderConfig {
    pub chunk_size: ChunkSize,
    pub max_payload_bytes: ByteLength,
}

impl Default for ChunkedReaderConfig {
    fn default() -> Self {
        Self {
            chunk_size: ChunkSize::KB_64,
            max_payload_bytes: ByteLength::new(1024 * 1024 * 500), // 500 MiB limit
        }
    }
}

/// Safe reader for working-tree files using owned buffers and never mmap.
pub struct SafeChunkReader;

impl SafeChunkReader {
    /// Read chunk by chunk from a reader without mmap.
    pub fn read_from_reader<R: Read>(
        request: CaptureRequest,
        reader: &mut R,
        config: ChunkedReaderConfig,
        expected_len: Option<u64>,
        cancel: &CancelFlag,
    ) -> Result<ChunkedCapture, SourceError> {
        if cancel.is_canceled() {
            return Err(SourceError::Canceled);
        }

        if let Some(expected) = expected_len
            && expected > config.max_payload_bytes.get()
        {
            return Err(SourceError::PayloadTooLarge);
        }

        let chunk_size_bytes = config.chunk_size.bytes();
        let max_payload = config.max_payload_bytes.get();

        let mut chunks = Vec::new();
        let mut total_read: u64 = 0;
        let mut chunk_index: u32 = 0;

        let mut temp_buf = vec![0u8; chunk_size_bytes];

        loop {
            if cancel.is_canceled() {
                return Err(SourceError::Canceled);
            }

            // Fill buffer up to chunk_size_bytes, handling short reads
            let mut chunk_filled = 0;
            while chunk_filled < chunk_size_bytes {
                match reader.read(&mut temp_buf[chunk_filled..]) {
                    Ok(0) => break, // EOF reached
                    Ok(n) => {
                        chunk_filled += n;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return Err(SourceError::RootUnavailable),
                }
            }

            if chunk_filled == 0 {
                // Natural EOF
                break;
            }

            let start_offset = total_read;
            total_read = total_read
                .checked_add(chunk_filled as u64)
                .ok_or(SourceError::RangeOutOfBounds)?;

            if total_read > max_payload {
                return Err(SourceError::PayloadTooLarge);
            }

            let end_offset = total_read;
            let byte_range = ByteRange::new(
                ByteOffset::new(start_offset),
                ByteOffset::new(end_offset),
            )
            .map_err(|_| SourceError::InvalidRange)?;

            let chunk_data: Arc<[u8]> = Arc::from(&temp_buf[..chunk_filled]);
            let chunk = SourceChunk::new(chunk_index, byte_range, chunk_data)?;
            chunks.push(chunk);
            chunk_index += 1;

            if chunk_filled < chunk_size_bytes {
                // Partial read indicates EOF
                break;
            }
        }

        if let Some(expected) = expected_len
            && total_read != expected
        {
            // Concurrent truncation or extension detected
            return Err(SourceError::ConcurrentModification);
        }

        ChunkedCapture::new(
            request,
            ByteLength::new(total_read),
            config.chunk_size,
            chunks,
        )
    }

    /// Read an authoritative working-tree file safely using owned chunk buffers.
    ///
    /// Never memory-maps mutable source. Queries metadata before and after the read
    /// to detect concurrent modification or truncation. Refuses non-regular files
    /// (FIFOs, sockets, device nodes) without blocking.
    pub fn read_file(
        request: CaptureRequest,
        path: &Path,
        config: ChunkedReaderConfig,
        cancel: &CancelFlag,
    ) -> Result<ChunkedCapture, SourceError> {
        if cancel.is_canceled() {
            return Err(SourceError::Canceled);
        }

        let mut file = File::open(path).map_err(|_| SourceError::RootUnavailable)?;
        let stat_before = file.metadata().map_err(|_| SourceError::RootUnavailable)?;

        if !stat_before.is_file() {
            return Err(SourceError::SpecialObject);
        }

        let initial_len = stat_before.len();
        if initial_len > config.max_payload_bytes.get() {
            return Err(SourceError::PayloadTooLarge);
        }

        let initial_modified = stat_before.modified().ok();

        let capture = Self::read_from_reader(
            request,
            &mut file,
            config,
            Some(initial_len),
            cancel,
        )?;

        // Re-stat file after read to verify consistency
        let stat_after = file.metadata().map_err(|_| SourceError::ConcurrentModification)?;
        if stat_after.len() != initial_len {
            return Err(SourceError::ConcurrentModification);
        }

        if let (Some(before_mtime), Ok(after_mtime)) = (initial_modified, stat_after.modified())
            && before_mtime != after_mtime
        {
            return Err(SourceError::ConcurrentModification);
        }

        Ok(capture)
    }
}

/// Registry of retained chunked captures enforcing snapshot immutability.
///
/// Ensures an evicted capture never silently re-reads live mutable source or
/// substitutes newer bytes under an old revision identity.
#[derive(Clone, Debug)]
pub struct RetainedCaptureStore {
    owner: ArenaOwnerId,
    captures: BTreeMap<(FileId, SourceRevision), ChunkedCapture>,
}

impl RetainedCaptureStore {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            captures: BTreeMap::new(),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn insert(&mut self, capture: ChunkedCapture) -> Result<(), SourceError> {
        if capture.request().file().owner() != self.owner
            || capture.request().revision().owner() != self.owner
        {
            return Err(SourceError::ForeignOwner);
        }

        let key = (capture.request().file(), capture.request().revision());
        if self.captures.contains_key(&key) {
            return Err(SourceError::CaptureAlreadyPresent);
        }

        self.captures.insert(key, capture);
        Ok(())
    }

    pub fn get(
        &self,
        file: FileId,
        revision: SourceRevision,
    ) -> Result<&ChunkedCapture, SourceError> {
        if file.owner() != self.owner || revision.owner() != self.owner {
            return Err(SourceError::ForeignOwner);
        }
        self.captures
            .get(&(file, revision))
            .ok_or(SourceError::CaptureUnavailable)
    }

    /// Evict a retained capture.
    pub fn evict(&mut self, file: FileId, revision: SourceRevision) -> bool {
        self.captures.remove(&(file, revision)).is_some()
    }

    /// Resolve an anchor range against a retained capture.
    ///
    /// If the capture is retained, returns the exact bytes.
    /// If the capture was evicted or never present, strictly returns `StaleCapture`
    /// rather than silently reading changed live bytes.
    pub fn resolve_anchor(
        &self,
        file: FileId,
        revision: SourceRevision,
        range: ByteRange,
    ) -> Result<ExactRangeResult, SourceError> {
        match self.get(file, revision) {
            Ok(capture) => capture.range_read(range),
            Err(SourceError::CaptureUnavailable) => Err(SourceError::StaleCapture),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn owner(id: u64) -> ArenaOwnerId {
        ArenaOwnerId::new(id).unwrap()
    }

    fn file(owner_id: ArenaOwnerId, val: u64) -> FileId {
        FileId::new(owner_id, val).unwrap()
    }

    fn revision(owner_id: ArenaOwnerId, val: u64) -> SourceRevision {
        SourceRevision::new(owner_id, val).unwrap()
    }

    fn request(owner_id: u64, file_id: u64, rev: u64) -> CaptureRequest {
        let o = owner(owner_id);
        CaptureRequest::new(file(o, file_id), revision(o, rev)).unwrap()
    }

    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap()
    }

    #[test]
    fn chunk_size_candidates_and_bounds() {
        assert_eq!(ChunkSize::from_candidate(16 * 1024).unwrap(), ChunkSize::KB_16);
        assert_eq!(ChunkSize::from_candidate(64 * 1024).unwrap(), ChunkSize::KB_64);
        assert_eq!(ChunkSize::from_candidate(256 * 1024).unwrap(), ChunkSize::KB_256);
        assert!(ChunkSize::from_candidate(32 * 1024).is_err());

        assert!(ChunkSize::bounded(1).is_ok());
        assert!(ChunkSize::bounded(16).is_ok());
        assert!(ChunkSize::bounded(1024 * 1024).is_ok());
        assert!(ChunkSize::bounded(0).is_err());
        assert!(ChunkSize::bounded(1024 * 1024 + 1).is_err());
    }

    #[test]
    fn empty_capture_validation() {
        let req = request(1, 1, 1);
        let cap = ChunkedCapture::new(
            req,
            ByteLength::new(0),
            ChunkSize::KB_64,
            Vec::new(),
        )
        .unwrap();

        assert_eq!(cap.total_length().get(), 0);
        assert_eq!(cap.chunk_count(), 0);
        assert_eq!(cap.digest(), ObservationDigest::observe(&[]));
    }

    #[test]
    fn chunk_creation_and_slicing() {
        let data = Arc::from(b"abcdefghijklmnop".to_vec().into_boxed_slice());
        let chunk = SourceChunk::new(0, range(0, 16), data).unwrap();

        assert_eq!(chunk.index(), 0);
        assert_eq!(chunk.len(), 16);
        assert_eq!(chunk.slice(range(2, 6)).unwrap(), b"cdef");
        assert_eq!(chunk.slice(range(0, 16)).unwrap(), b"abcdefghijklmnop");
        assert_eq!(chunk.slice(range(5, 5)), Err(SourceError::InvalidRange));
        assert_eq!(chunk.slice(range(10, 20)), Err(SourceError::RangeOutOfBounds));
    }

    #[test]
    fn multi_chunk_capture_and_range_reads() {
        let req = request(1, 10, 1);
        let chunk_size = ChunkSize::bounded(16).unwrap();

        let chunk0_data = Arc::from(b"0123456789ABCDEF".to_vec().into_boxed_slice());
        let chunk1_data = Arc::from(b"GHIJKLMNOPQRSTUV".to_vec().into_boxed_slice());
        let chunk2_data = Arc::from(b"WXYZ".to_vec().into_boxed_slice());

        let chunk0 = SourceChunk::new(0, range(0, 16), chunk0_data).unwrap();
        let chunk1 = SourceChunk::new(1, range(16, 32), chunk1_data).unwrap();
        let chunk2 = SourceChunk::new(2, range(32, 36), chunk2_data).unwrap();

        let cap = ChunkedCapture::new(
            req,
            ByteLength::new(36),
            chunk_size,
            vec![chunk0, chunk1, chunk2],
        )
        .unwrap();

        assert_eq!(cap.chunk_count(), 3);
        assert_eq!(
            cap.digest(),
            ObservationDigest::observe(b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ")
        );

        // Contiguous read within chunk 0
        let res0 = cap.range_read(range(4, 8)).unwrap();
        assert!(res0.is_contiguous());
        assert_eq!(res0.bytes(), b"4567");

        // Contiguous read within chunk 1
        let res1 = cap.range_read(range(18, 22)).unwrap();
        assert!(res1.is_contiguous());
        assert_eq!(res1.bytes(), b"IJKL");

        // Cross-chunk read spanning chunk 0 and chunk 1
        let res_cross = cap.range_read(range(12, 20)).unwrap();
        assert!(!res_cross.is_contiguous());
        assert_eq!(res_cross.bytes(), b"CDEFGHIJ");

        // Cross-chunk read spanning all 3 chunks
        let res_all = cap.range_read(range(10, 35)).unwrap();
        assert!(!res_all.is_contiguous());
        assert_eq!(res_all.bytes(), b"ABCDEFGHIJKLMNOPQRSTUVWXY");

        // Out of bounds
        assert_eq!(cap.range_read(range(30, 40)), Err(SourceError::RangeOutOfBounds));
    }

    #[test]
    fn cross_chunk_utf8_multibyte_boundary() {
        // "🦀" is 4 bytes: 0xF0, 0x9F, 0xA6, 0x80
        // We split across an 8-byte chunk boundary:
        // Chunk 0: 6 ASCII bytes + 2 bytes of crab
        // Chunk 1: 2 bytes of crab + 6 ASCII bytes
        let mut c0_bytes = b"prefix".to_vec();
        let crab = "🦀".as_bytes();
        c0_bytes.push(crab[0]);
        c0_bytes.push(crab[1]);

        let mut c1_bytes = vec![crab[2], crab[3]];
        c1_bytes.extend_from_slice(b"suffix");

        let chunk_size = ChunkSize::bounded(8).unwrap();
        let chunk0 = SourceChunk::new(0, range(0, 8), Arc::from(c0_bytes.into_boxed_slice())).unwrap();
        let chunk1 = SourceChunk::new(1, range(8, 16), Arc::from(c1_bytes.into_boxed_slice())).unwrap();

        let cap = ChunkedCapture::new(
            request(2, 20, 1),
            ByteLength::new(16),
            chunk_size,
            vec![chunk0, chunk1],
        )
        .unwrap();

        // Read range [6, 10) spanning the split UTF-8 character
        let read = cap.range_read(range(6, 10)).unwrap();
        assert_eq!(read.bytes(), "🦀".as_bytes());
        assert_eq!(std::str::from_utf8(read.bytes()).unwrap(), "🦀");
    }

    #[test]
    fn cross_chunk_crlf_boundary() {
        // CRLF split: '\r' at offset 7 (end of chunk 0), '\n' at offset 8 (start of chunk 1)
        let chunk_size = ChunkSize::bounded(8).unwrap();
        let c0_bytes = Arc::from(b"line 1-\r".to_vec().into_boxed_slice());
        let c1_bytes = Arc::from(b"\nline 2-".to_vec().into_boxed_slice());

        let chunk0 = SourceChunk::new(0, range(0, 8), c0_bytes).unwrap();
        let chunk1 = SourceChunk::new(1, range(8, 16), c1_bytes).unwrap();

        let cap = ChunkedCapture::new(
            request(3, 30, 1),
            ByteLength::new(16),
            chunk_size,
            vec![chunk0, chunk1],
        )
        .unwrap();

        // Slicing [7, 9) yields CRLF
        let crlf = cap.range_read(range(7, 9)).unwrap();
        assert_eq!(crlf.bytes(), b"\r\n");
    }

    #[test]
    fn safe_chunk_reader_from_reader_with_payload_limit() {
        let data = b"The quick brown fox jumps over the lazy dog.";
        let mut cursor = Cursor::new(data);
        let req = request(4, 40, 1);
        let cancel = CancelFlag::new();

        let config = ChunkedReaderConfig {
            chunk_size: ChunkSize::bounded(16).unwrap(),
            max_payload_bytes: ByteLength::new(100),
        };

        let cap = SafeChunkReader::read_from_reader(
            req,
            &mut cursor,
            config,
            Some(data.len() as u64),
            &cancel,
        )
        .unwrap();

        assert_eq!(cap.total_length().get(), data.len() as u64);
        assert_eq!(cap.chunk_count(), 3); // 16 + 16 + 12 = 44 bytes

        // Exceeded limit fails
        let small_config = ChunkedReaderConfig {
            chunk_size: ChunkSize::bounded(16).unwrap(),
            max_payload_bytes: ByteLength::new(20),
        };
        let mut cursor2 = Cursor::new(data);
        assert_eq!(
            SafeChunkReader::read_from_reader(req, &mut cursor2, small_config, Some(data.len() as u64), &cancel),
            Err(SourceError::PayloadTooLarge)
        );
    }

    #[test]
    fn safe_chunk_reader_detects_concurrent_truncation() {
        let data = b"1234567890abcdefghijklmnopqrstuvwxyz";
        let mut cursor = Cursor::new(data);
        let req = request(5, 50, 1);
        let cancel = CancelFlag::new();

        let config = ChunkedReaderConfig {
            chunk_size: ChunkSize::bounded(16).unwrap(),
            max_payload_bytes: ByteLength::new(100),
        };

        // If expected length is 50 but reader only has 36 bytes, truncation detected!
        let result = SafeChunkReader::read_from_reader(
            req,
            &mut cursor,
            config,
            Some(50),
            &cancel,
        );
        assert_eq!(result, Err(SourceError::ConcurrentModification));
    }

    #[test]
    fn cancellation_halts_chunked_reader() {
        let data = b"some data here";
        let mut cursor = Cursor::new(data);
        let req = request(6, 60, 1);
        let cancel = CancelFlag::new();
        cancel.cancel();

        let config = ChunkedReaderConfig::default();
        assert_eq!(
            SafeChunkReader::read_from_reader(req, &mut cursor, config, None, &cancel),
            Err(SourceError::Canceled)
        );
    }

    #[test]
    fn negative_control_evicted_capture_never_reads_live_source() {
        let o = owner(7);
        let f = file(o, 100);
        let rev1 = revision(o, 1);
        let rev2 = revision(o, 2);

        let mut store = RetainedCaptureStore::new(o);

        // Insert capture for revision 1
        let req1 = CaptureRequest::new(f, rev1).unwrap();
        let chunk1 = SourceChunk::new(
            0,
            range(0, 16),
            Arc::from(b"revision-1-bytes".to_vec().into_boxed_slice()),
        )
        .unwrap();
        let cap1 = ChunkedCapture::new(
            req1,
            ByteLength::new(16),
            ChunkSize::bounded(16).unwrap(),
            vec![chunk1],
        )
        .unwrap();
        store.insert(cap1).unwrap();

        // Querying revision 1 works
        let res1 = store.resolve_anchor(f, rev1, range(0, 10)).unwrap();
        assert_eq!(res1.bytes(), b"revision-1");

        // Evict revision 1
        assert!(store.evict(f, rev1));

        // Insert capture for revision 2
        let req2 = CaptureRequest::new(f, rev2).unwrap();
        let chunk2 = SourceChunk::new(
            0,
            range(0, 16),
            Arc::from(b"revision-2-bytes".to_vec().into_boxed_slice()),
        )
        .unwrap();
        let cap2 = ChunkedCapture::new(
            req2,
            ByteLength::new(16),
            ChunkSize::bounded(16).unwrap(),
            vec![chunk2],
        )
        .unwrap();
        store.insert(cap2).unwrap();

        // Negative control: Querying evicted revision 1 MUST fail with StaleCapture.
        // It must NEVER silently return revision 2 or substitute new bytes!
        assert_eq!(
            store.resolve_anchor(f, rev1, range(0, 10)),
            Err(SourceError::StaleCapture)
        );

        // Revision 2 is cleanly isolated
        let res2 = store.resolve_anchor(f, rev2, range(0, 10)).unwrap();
        assert_eq!(res2.bytes(), b"revision-2");
    }
}
