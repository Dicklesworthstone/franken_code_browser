#![forbid(unsafe_code)]

//! Small schema-specific encoder, not a general JSON parser or serializer.
//! Full-width integers are decimal STRINGS; Unix paths carry reversible bytes.
//! Build one bounded document before stdout, never interleave partial JSON
//! documents. Failed writes cannot be repaired by appending a second document.

use std::{io::{self, Write}, mem::size_of, path::Path};
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget};
use fcb::source::RawPath;

pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const PATH_SCRATCH_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputError { Limit, Allocation, UnsupportedPath }
impl OutputError {
    pub const fn code(self) -> &'static str {
        match self { Self::Limit => "CLI_OUTPUT_LIMIT", Self::Allocation => "CLI_OUTPUT_ADMISSION",
            Self::UnsupportedPath => "CLI_PATH_ENCODING_UNSUPPORTED" }
    }
}
impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for OutputError {}

pub struct Output {
    bytes: Vec<u8>,
    limit: usize,
    _lease: fcb::source::confined::range::ExtentErrorMarker,
}
