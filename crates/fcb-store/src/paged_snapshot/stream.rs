#![forbid(unsafe_code)]

//! Source-length-independent access to one member of a retained archive.
//!
//! Bytes returned by MemberReader are provisional until its complete stream has
//! matched the directory's SHA-256 digest. Consumers must keep derived output
//! private until finish returns a VerifiedMemberIdentity. Neither dropping an
//! unfinished stream nor reaching a result limit verifies its unread suffix.
//! The session borrows the original input and directory; it never reopens a path.

use super::{
    MAX_SNAPSHOT_READ_CALLS, PagedMemberData, PagedSnapshot, PagedSnapshotError,
    SNAPSHOT_READ_BYTES, SnapshotDirectory, SnapshotIoStats,
};
use crate::{Sha256, Sha256Digest};
use std::io::{self, Read, Seek, SeekFrom};

/// Split borrow of an archive's immutable directory and exclusive input/stats.
/// Moving this session into a member reader requires no self-referential borrow
/// and does not move the host's actual archive handle.
pub struct SnapshotReadSession<'archive, R: Read + Seek> {
    input: &'archive mut R,
    directory: &'archive SnapshotDirectory,
    loads: &'archive mut SnapshotIoStats,
}
impl<R: Read + Seek> PagedSnapshot<R> {
    /// Begin an explicit source-reading session. Construction performs no I/O
    /// or allocation. Dropping a session or unfinished member releases the
    /// exclusive input borrow, allowing a later explicit query to start anew.
    pub fn read_session(&mut self) -> SnapshotReadSession<'_, R> {
        SnapshotReadSession {
            input: &mut self.input,
            directory: &self.directory,
            loads: &mut self.loads,
        }
    }
}
impl<'archive, R: Read + Seek> SnapshotReadSession<'archive, R> {
    pub const fn directory(&self) -> &'archive SnapshotDirectory {
        self.directory
    }
    pub fn load_stats(&self) -> SnapshotIoStats {
        *self.loads
    }

    /// Select only a member of this session's verified/pinned directory. The
    /// first nonempty read seeks to its saved offset; construction does not read
    /// or seek. An empty member can verify without touching the input.
    pub fn member(self, ordinal: usize) -> Result<MemberReader<'archive, R>, PagedSnapshotError> {
        let member = self
            .directory
            .member(ordinal)
            .ok_or(PagedSnapshotError::Missing)?;
        let PagedMemberData::Captured {
            archive_offset,
            byte_length,
            digest,
        } = member.data
        else {
            return Err(PagedSnapshotError::Missing);
        };
        let mut reader = MemberReader {
            session: self,
            ordinal,
            archive_offset,
            byte_length,
            digest,
            hash: Some(Sha256::new()),
            position: 0,
            positioned: false,
            state: MemberReadState::Pending,
            stats: SnapshotIoStats::default(),
        };
        if byte_length == 0 {
            reader.verify()?;
        }
        Ok(reader)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberReadState {
    Pending,
    Verified,
    Failed(PagedSnapshotError),
}

/// Evidence that exactly one complete observed member stream matched its saved
/// digest. It carries identity only, not source bytes or permission to substitute
/// later reads. Its private fields cannot be manufactured by a caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedMemberIdentity {
    ordinal: usize,
    byte_length: usize,
    archive: Sha256Digest,
    source: Sha256Digest,
}
impl VerifiedMemberIdentity {
    pub const fn ordinal(self) -> usize {
        self.ordinal
    }
    pub const fn byte_length(self) -> usize {
        self.byte_length
    }
    pub const fn archive_digest(self) -> Sha256Digest {
        self.archive
    }
    pub const fn source_digest(self) -> Sha256Digest {
        self.source
    }
}

/// One incremental SHA-256 pass over the original archive input, with no owned
/// payload buffer. Read makes at most one underlying call, offers at most 64 KiB,
/// and never crosses the selected member boundary. Interrupted calls spend the
/// hard call allowance and are returned to the caller for bounded retry.
/// A blocking host Read/Seek has no invented wall-clock deadline.
pub struct MemberReader<'archive, R: Read + Seek> {
    session: SnapshotReadSession<'archive, R>,
    ordinal: usize,
    archive_offset: u64,
    byte_length: usize,
    digest: Sha256Digest,
    hash: Option<Sha256>,
    position: usize,
    positioned: bool,
    state: MemberReadState,
    stats: SnapshotIoStats,
}
impl<'archive, R: Read + Seek> MemberReader<'archive, R> {
    pub const fn state(&self) -> MemberReadState {
        self.state
    }
    pub const fn stats(&self) -> SnapshotIoStats {
        self.stats
    }
    pub const fn byte_length(&self) -> usize {
        self.byte_length
    }
    pub const fn remaining_bytes(&self) -> usize {
        self.byte_length - self.position
    }
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Cancellation never verifies or drains unread bytes. Terminal reads stay
    /// terminal, and a canceled stream performs no later I/O.
    pub fn cancel(&mut self) {
        if self.state == MemberReadState::Pending {
            self.state = MemberReadState::Failed(PagedSnapshotError::Canceled);
        }
    }

    /// Return the reusable session only after the entire observed member has
    /// matched its digest. Pending/failed streams cannot produce verification
    /// evidence; dropping the error still releases the original archive borrow.
    pub fn finish(
        self,
    ) -> Result<(SnapshotReadSession<'archive, R>, VerifiedMemberIdentity), PagedSnapshotError>
    {
        match self.state {
            MemberReadState::Pending => Err(PagedSnapshotError::Pending),
            MemberReadState::Failed(error) => Err(error),
            MemberReadState::Verified => {
                let identity = VerifiedMemberIdentity {
                    ordinal: self.ordinal,
                    byte_length: self.byte_length,
                    archive: self.session.directory.digest(),
                    source: self.digest,
                };
                Ok((self.session, identity))
            }
        }
    }

    fn verify(&mut self) -> Result<(), PagedSnapshotError> {
        let digest = self
            .hash
            .take()
            .ok_or(PagedSnapshotError::Member)?
            .finalize();
        if digest != self.digest {
            self.state = MemberReadState::Failed(PagedSnapshotError::Changed);
            return Err(PagedSnapshotError::Changed);
        }
        self.state = MemberReadState::Verified;
        self.stats.loaded_members = 1;
        self.session.loads.loaded_members = self.session.loads.loaded_members.saturating_add(1);
        Ok(())
    }

    fn fail(&mut self, error: PagedSnapshotError) -> io::Error {
        self.state = MemberReadState::Failed(error);
        io_error(error)
    }
}
impl<R: Read + Seek> Read for MemberReader<'_, R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        match self.state {
            MemberReadState::Verified => return Ok(0),
            MemberReadState::Failed(error) => return Err(io_error(error)),
            MemberReadState::Pending => {}
        }
        if into.is_empty() {
            return Ok(0);
        }
        if self.stats.read_calls >= MAX_SNAPSHOT_READ_CALLS {
            return Err(self.fail(PagedSnapshotError::ReadCallLimit));
        }
        if !self.positioned {
            match self
                .session
                .input
                .seek(SeekFrom::Start(self.archive_offset))
            {
                Ok(position) if position == self.archive_offset => self.positioned = true,
                _ => return Err(self.fail(PagedSnapshotError::Io)),
            }
        }
        let count = into
            .len()
            .min(SNAPSHOT_READ_BYTES)
            .min(self.remaining_bytes());
        self.stats.read_calls += 1;
        self.session.loads.read_calls = self.session.loads.read_calls.saturating_add(1);
        match self.session.input.read(&mut into[..count]) {
            Ok(0) => Err(self.fail(PagedSnapshotError::Format)),
            Ok(read) if read <= count => {
                self.stats.bytes_read += read as u64;
                self.session.loads.bytes_read =
                    self.session.loads.bytes_read.saturating_add(read as u64);
                self.position += read;
                let Some(hash) = self.hash.as_mut() else {
                    return Err(self.fail(PagedSnapshotError::Member));
                };
                hash.update(&into[..read]);
                if self.position == self.byte_length {
                    self.verify().map_err(io_error)?;
                }
                Ok(read)
            }
            Ok(_) => Err(self.fail(PagedSnapshotError::InvalidReadCount)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
            Err(_) => Err(self.fail(PagedSnapshotError::Io)),
        }
    }
}
fn io_error(error: PagedSnapshotError) -> io::Error {
    let kind = match error {
        PagedSnapshotError::Canceled => io::ErrorKind::Interrupted,
        PagedSnapshotError::Format => io::ErrorKind::UnexpectedEof,
        PagedSnapshotError::Io => io::ErrorKind::Other,
        _ => io::ErrorKind::InvalidData,
    };
    io::Error::from(kind)
}
