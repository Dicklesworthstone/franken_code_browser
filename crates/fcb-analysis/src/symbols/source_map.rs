#![forbid(unsafe_code)]

//! Symbol-specific limits and errors over the shared bounded analysis map.
//! The implementation stays in one place; this adapter preserves the existing
//! declaration recognizers' contracts and aggregate candidate-name allowance.

use fcb_core::ByteRange;
use fcb_source::DetectedEncoding;
use crate::source_map::{SourceMap as AnalysisSourceMap, SourceMapError};
use super::{MAX_SYMBOL_SOURCE_BYTES, SymbolError};

pub(super) struct SourceMap<'source>(AnalysisSourceMap<'source>);

fn error(error: SourceMapError) -> SymbolError {
    match error {
        SourceMapError::SourceLimit => SymbolError::SourceLimit,
        SourceMapError::UnsupportedEncoding => SymbolError::UnsupportedEncoding,
        SourceMapError::InvalidEvidence => SymbolError::InvalidEvidence,
        SourceMapError::ResourceDenied => SymbolError::ResourceDenied,
        SourceMapError::Canceled => SymbolError::Canceled,
        SourceMapError::ComplexityLimit => SymbolError::ComplexityLimit,
    }
}

impl<'source> SourceMap<'source> {
    pub(super) fn decoded_capacity(source_bytes: usize, encoding: DetectedEncoding)
        -> Result<usize, SymbolError> {
        AnalysisSourceMap::decoded_capacity(source_bytes, encoding, MAX_SYMBOL_SOURCE_BYTES).map_err(error)
    }

    pub(super) fn workspace_bytes(source_bytes: usize, decoded_capacity: usize)
        -> Result<usize, SymbolError> {
        AnalysisSourceMap::workspace_bytes(source_bytes, decoded_capacity).map_err(error)
    }

    pub(super) fn build(source: &'source [u8], encoding: DetectedEncoding,
        canceled: &mut impl FnMut() -> bool) -> Result<Self, SymbolError> {
        AnalysisSourceMap::build(source, encoding, MAX_SYMBOL_SOURCE_BYTES,
            MAX_SYMBOL_SOURCE_BYTES, canceled).map(Self).map_err(error)
    }

    pub(super) fn decoded_text(&self) -> &str { self.0.decoded_text() }
    pub(super) fn name_buffer(&self, capacity: usize) -> Result<String, SymbolError> {
        self.0.name_buffer(capacity).map_err(error)
    }
    pub(super) fn copy_name(&self, value: &str) -> Result<String, SymbolError> {
        self.0.copy_name(value).map_err(error)
    }
    pub(super) fn mapped(&self, start: u64, end: u64) -> Result<ByteRange, SymbolError> {
        self.0.mapped(start, end).map_err(error)
    }
}
