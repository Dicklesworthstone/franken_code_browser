#![forbid(unsafe_code)]

//! Image acquisition from an explicitly opened immutable saved repository.
//! The existing retained display owns image occurrences and path resolution;
//! the archive verifies member payloads and the reader retains bounded chunks.
//! Member labels never become native filesystem paths or root capabilities.

use fcb::search::paged_snapshot::PagedMemberData;
use fcb::source::RawPath;
use crate::host::reader::{DocumentImageError, ReaderSession, ReaderSessionError};
use super::{check, HostResponse, SavedRepositoryError, SavedRepositorySession};

impl SavedRepositorySession {
    /// Bind this reader's exact retained source to one captured archive member.
    /// Directory metadata and the reader's admitted digest suffice: no source
    /// file or member payload is reopened and no live root grant is created.
    pub fn authorize_document_images(&mut self, reader: &mut ReaderSession,
        source_member: usize, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?;
        self.validate_document_image_origin(reader, source_member)?;
        let member = self.archive.directory().member(source_member)
            .ok_or(SavedRepositoryError::MissingHit)?;
        let PagedMemberData::Captured { digest, .. } = member.data else {
            return Err(image_error(DocumentImageError::OriginMismatch));
        };
        let response = reader.authorize_saved_document_images(generation, self.owner(),
            self.archive_digest(), source_member, member.path, digest, &mut canceled)?;
        check(&mut canceled)?;
        Ok(response)
    }

    /// Acquire only the destination of this exact parser-owned display item.
    /// Captured bytes are retained once per occurrence in the ordinary reader
    /// image cache. Cache replay and chunk reads do not reread the archive.
    pub fn prepare_document_image(&mut self, reader: &mut ReaderSession,
        source_member: usize, generation: u64, item_index: usize, request_id: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?;
        self.validate_document_image_origin(reader, source_member)?;
        let owner = self.owner();
        let archive = self.archive_digest();
        let resolved = reader.saved_document_image_path(generation, owner, archive,
            source_member, item_index, request_id)?;
        let data = if let Some((path, limit)) = resolved {
            let ordinal = self.archive.directory().find_path(path.as_bytes())
                .ok_or_else(|| image_error(DocumentImageError::MissingSavedMember))?;
            let member = self.archive.directory().member(ordinal)
                .ok_or_else(|| image_error(DocumentImageError::MissingSavedMember))?;
            let PagedMemberData::Captured { byte_length, .. } = member.data else {
                return Err(image_error(DocumentImageError::UnavailableSavedMember));
            };
            if byte_length == 0 || byte_length > limit {
                return Err(image_error(DocumentImageError::InvalidLimits));
            }
            let [allocation] = self.allocations()?;
            let before = self.archive.load_stats();
            let verified = self.archive.load(ordinal, &self.budget, allocation, &mut canceled)?;
            let calls = self.archive.load_stats().read_calls - before.read_calls;
            check(&mut canceled)?;
            Some((verified, calls))
        } else { None };
        let response = reader.retain_saved_document_image(generation, owner, archive,
            source_member, item_index, request_id, data, &mut canceled)?;
        check(&mut canceled)?;
        Ok(response)
    }

    fn validate_document_image_origin(&self, reader: &ReaderSession, ordinal: usize)
        -> Result<(), SavedRepositoryError> {
        let member = self.archive.directory().member(ordinal)
            .ok_or_else(|| image_error(DocumentImageError::OriginMismatch))?;
        let PagedMemberData::Captured { byte_length, digest, .. } = member.data else {
            return Err(image_error(DocumentImageError::OriginMismatch));
        };
        if byte_length != reader.capture().bytes().len() || digest != reader.capture_sha256()
            || RawPath::from_path(reader.path()).as_bytes() != member.path {
            return Err(image_error(DocumentImageError::OriginMismatch));
        }
        Ok(())
    }
}
fn image_error(error: DocumentImageError) -> SavedRepositoryError {
    ReaderSessionError::DocumentImage(error).into()
}
