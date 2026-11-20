#![deny(unsafe_op_in_unsafe_fn)]

//! Descriptor lifetime/marshaling only. The shared checkpoint engine owns the
//! streaming checksum and format contract. No desk registry or source path is
//! involved, so verification cannot consume reader slots or replace user state.

/// Verify a previously saved checkpoint through the exact open native file.
/// Returns one only for full integrity; every rejection/panic returns zero.
/// Worker work, bounded to 64 MiB and 64 KiB scratch. No file offset is changed.
/// The caller's descriptor remains open and owned by the caller.
/// # Safety
/// For admitted lengths, expected_digest addresses 32 initialized readable
/// bytes, stable until return. A nonnegative descriptor is valid and readable
/// and must not be closed or reused concurrently with this call. Its underlying
/// file may change: changed size, metadata or receipt-bound bytes are rejected.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_desk_checkpoint_verify_fd(
    descriptor: i32,
    expected_bytes: u64,
    expected_digest: *const u8,
) -> u8 {
    if descriptor < 0 || expected_digest.is_null()
        || !(56..=fcb::ui::reading_panes::desk::checkpoint::MAX_CHECKPOINT_BYTES as u64)
            .contains(&expected_bytes)
    {
        return 0;
    }
    #[cfg(unix)]
    {
        std::panic::catch_unwind(|| {
            use std::{fs::File, os::fd::BorrowedFd};
            use fcb::store::envelope::Sha256Digest;
            use fcb::ui::reading_panes::desk::checkpoint::integrity::verify_checkpoint_integrity;

            // Borrow under the caller's explicit lifetime contract, then retain
            // our own descriptor. Positional reads preserve the shared cursor.
            let borrowed = unsafe { BorrowedFd::borrow_raw(descriptor) };
            let owned = match borrowed.try_clone_to_owned() { Ok(fd) => fd, Err(_) => return 0 };
            let file = File::from(owned);
            let before = match file.metadata() { Ok(info) => info, Err(_) => return 0 };
            if !before.is_file() || before.len() != expected_bytes { return 0; }
            let bytes = unsafe { std::slice::from_raw_parts(expected_digest, 32) };
            let digest = Sha256Digest::new(bytes.try_into().expect("fixed digest"));
            let mut reader = PositionedReader { file: &file, offset: 0 };
            if verify_checkpoint_integrity(&mut reader, expected_bytes, digest, || false).is_err() {
                return 0;
            }
            let after = match file.metadata() { Ok(info) => info, Err(_) => return 0 };
            u8::from(same_observation(&before, &after))
        }).unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(unix)]
struct PositionedReader<'a> { file: &'a std::fs::File, offset: u64 }
#[cfg(unix)]
impl std::io::Read for PositionedReader<'_> {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        use std::os::unix::fs::FileExt;
        let n = self.file.read_at(into, self.offset)?;
        self.offset = self.offset.checked_add(n as u64)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        Ok(n)
    }
}
#[cfg(unix)]
fn same_observation(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    b.is_file() && a.dev() == b.dev() && a.ino() == b.ino() && a.len() == b.len()
        && a.mtime() == b.mtime() && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs::{File, OpenOptions}, io::{Read, Seek, SeekFrom, Write},
        os::{fd::AsRawFd, unix::fs::FileExt}, sync::atomic::{AtomicU64, Ordering}};
    use fcb::store::envelope::{EnvelopeSchema, EnvelopeWriter, Sha256Digest};

    fn checkpoint() -> (Vec<u8>, Sha256Digest) {
        let mut out = EnvelopeWriter::new(EnvelopeSchema::with_magic(*b"FCBK", 0x4b53_4544, 1, 1));
        for n in [0, 0, 0, 0, u64::MAX, 0] { out.put_u64(n); }
        let bytes = out.finish();
        let digest = Sha256Digest::new(bytes[bytes.len() - 32..].try_into().unwrap());
        (bytes, digest)
    }
    fn file(bytes: &[u8]) -> File {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let name = format!("fcb-desk-integrity-{}-{}-{}.fcbdesk", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed));
        let mut file = OpenOptions::new().read(true).write(true).create_new(true)
            .open(std::env::temp_dir().join(name)).unwrap();
        file.write_all(bytes).unwrap();
        file
    }
    #[test]
    fn descriptor_verification_preserves_ownership_cursor_and_original_bytes() {
        let (bytes, digest) = checkpoint();
        let mut file = file(&bytes);
        file.seek(SeekFrom::Start(7)).unwrap();
        assert_eq!(unsafe { fcb_desk_checkpoint_verify_fd(file.as_raw_fd(), bytes.len() as u64,
            digest.as_bytes().as_ptr()) }, 1);
        assert_eq!(file.stream_position().unwrap(), 7);
        let mut next = [0; 1]; file.read_exact(&mut next).unwrap();
        assert_eq!(next[0], bytes[7], "the caller's handle remains usable and positioned");
    }
    #[test]
    fn changed_body_cannot_pass_with_an_unchanged_trailer() {
        let (bytes, digest) = checkpoint();
        let file = file(&bytes);
        file.write_at(&[1], 24).unwrap();
        let mut trailer = [0; 32];
        file.read_at(&mut trailer, (bytes.len() - 32) as u64).unwrap();
        assert_eq!(&trailer, digest.as_bytes());
        assert_eq!(unsafe { fcb_desk_checkpoint_verify_fd(file.as_raw_fd(), bytes.len() as u64,
            digest.as_bytes().as_ptr()) }, 0);
    }
    #[test]
    fn wrong_length_digest_and_non_regular_descriptors_are_refused() {
        let (bytes, digest) = checkpoint();
        let file = file(&bytes);
        assert_eq!(unsafe { fcb_desk_checkpoint_verify_fd(file.as_raw_fd(), bytes.len() as u64 + 1,
            digest.as_bytes().as_ptr()) }, 0);
        assert_eq!(unsafe { fcb_desk_checkpoint_verify_fd(file.as_raw_fd(), bytes.len() as u64,
            [0; 32].as_ptr()) }, 0);
        let directory = File::open(std::env::temp_dir()).unwrap();
        assert_eq!(unsafe { fcb_desk_checkpoint_verify_fd(directory.as_raw_fd(), bytes.len() as u64,
            digest.as_bytes().as_ptr()) }, 0);
        assert_eq!(unsafe { fcb_desk_checkpoint_verify_fd(-1, bytes.len() as u64, std::ptr::null()) }, 0);
        assert_eq!(unsafe { fcb_desk_checkpoint_verify_fd(file.as_raw_fd(), u64::MAX, std::ptr::null()) }, 0);
    }
}
