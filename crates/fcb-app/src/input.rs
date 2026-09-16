#![forbid(unsafe_code)]

//! Explicit CLI I/O over the public source-range service. No directory walk,
//! shell, cache, watcher, source write or background runtime is started.
//!
//! The named file is the ONLY admitted source. The parent path is informative,
//! not a recursively granted workspace or a claim of ancestor confinement.
//! Leaf no-follow/nonblocking flags and opened-object checks close the ordinary
//! final-component symlink/FIFO race; ancestor replacement still requires the
//! separate qualified native root service before a sandbox claim is permitted.

use std::{fs::{self, File, Metadata, OpenOptions}, io::{self, Read}, path::{Path, PathBuf}};
use fcb::{ByteLength, ByteOffset, ByteRange};
use fcb::search::{CaptureRequest, ExtentReadState, ExtentStepBudget, ExtentWindowRequest,
    FileRangeReader, ObservedExtent, ResourceBudget};
use fcb_core::ResourceLease;
use crate::{AppError, allocation, file_id, owner, revision};
use crate::args::Arguments;

pub const NATIVE_FILE_SUPPORTED: bool = cfg!(any(target_os = "macos",
    all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))));
const MAX_IO_CALLS: usize = 4096;

pub(crate) struct Loaded {
    pub extent: ObservedExtent,
    pub visible: ByteRange,
    pub path: Option<PathBuf>,
    pub read_calls: u64,
    _paths: ResourceLease,
}

pub(crate) fn absolute(path: &Path) -> Result<PathBuf, AppError> {
    let absolute = if path.is_absolute() { path.to_path_buf() }
        else { std::env::current_dir().map_err(|_| AppError::Io)?.join(path) };
    if absolute.as_os_str().len() > 16_384 { return Err(AppError::InputLimit); }
    Ok(absolute)
}

/// Open one explicitly named regular file. Values are system ABI constants,
/// not an imported third-party implementation. Header references are retained
/// in the CLI README. Unknown target ABIs refuse rather than guessing flags.
pub(crate) fn open_regular(path: &Path) -> Result<(File, Metadata), AppError> {
    if !NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform); }
    let before = fs::symlink_metadata(path).map_err(|_| AppError::Io)?;
    if before.file_type().is_symlink() { return Err(AppError::Symlink); }
    if before.is_dir() { return Err(AppError::Directory); }
    if !before.is_file() { return Err(AppError::Special); }
    #[cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        // O_NOFOLLOW | O_NONBLOCK | O_NOCTTY. Read-only; no O_CREAT/O_TRUNC.
        #[cfg(target_os = "macos")]
        const FLAGS: i32 = 0x0000_0100 | 0x0000_0004 | 0x0002_0000;
        #[cfg(target_os = "linux")]
        const FLAGS: i32 = (1 << 17) | (1 << 11) | (1 << 8);
        let file = OpenOptions::new().read(true).custom_flags(FLAGS).open(path).map_err(|_| AppError::Io)?;
        let opened = file.metadata().map_err(|_| AppError::Io)?;
        if !opened.is_file() { return Err(AppError::Special); }
        if before.dev() != opened.dev() || before.ino() != opened.ino() { return Err(AppError::SourceChanged); }
        Ok((file, opened))
    }
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))))]
    { Err(AppError::UnsupportedPlatform) }
}

pub(crate) fn load(args: &Arguments, stdin: &mut impl Read, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<Loaded, AppError> {
    if canceled() { return Err(AppError::Canceled); }
    let paths = budget.try_reserve_managed(owner(), allocation(10), ByteLength::new(128 * 1024))
        .map_err(|_| AppError::Admission)?;
    if args.stdin { return read_stdin(args.bytes, stdin, budget, paths, canceled); }
    let path = absolute(args.file.as_deref().ok_or(AppError::InvalidRange)?)?;
    let (file, _) = open_regular(&path)?;
    let mut reader = FileRangeReader::new(file_id(), file)?;
    let length = reader.observed_length()?;
    let window = ExtentWindowRequest::new(ByteOffset::new(args.offset), args.bytes, length)?;
    let mut request = CaptureRequest::new(file_id(), revision()).map_err(|_| AppError::InvalidRange)?;
    if !window.capture.is_empty() {
        request = request.with_range(window.capture).map_err(|_| AppError::InvalidRange)?;
    }
    let mut read = reader.begin(request, budget, allocation(11))?;
    while read.state() == ExtentReadState::Pending {
        if read.stats().read_calls >= MAX_IO_CALLS as u64 { return Err(AppError::IoCallLimit); }
        let calls = (MAX_IO_CALLS as u64 - read.stats().read_calls).min(32) as usize;
        read.step(ExtentStepBudget { max_bytes: 64 * 1024, max_calls: calls }, &mut *canceled)?;
    }
    let read_calls = read.stats().read_calls;
    let extent = read.finish(&mut *canceled)?;
    let end = window.visible.end().get().min(extent.range().end().get());
    let start = window.visible.start().get().min(end).max(extent.range().start().get());
    let visible = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| AppError::InvalidRange)?;
    Ok(Loaded { extent, visible, path: Some(path), read_calls, _paths: paths })
}

fn read_stdin(limit: usize, input: &mut impl Read, budget: &ResourceBudget, paths: ResourceLease,
    canceled: &mut impl FnMut() -> bool) -> Result<Loaded, AppError> {
    // Stdin has no declared total length. Only EOF establishes a complete
    // observation. Never present a capped prefix as an invented whole file.
    let capacity = limit.checked_add(1).ok_or(AppError::InputLimit)?;
    let _scratch = budget.try_reserve_managed(owner(), allocation(12), ByteLength::new(capacity as u64))
        .map_err(|_| AppError::Admission)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if bytes.capacity() > capacity { return Err(AppError::Admission); }
    bytes.resize(capacity, 0);
    let mut filled: usize = 0;
    let mut calls = 0;
    loop {
        if canceled() { return Err(AppError::Canceled); }
        if calls == MAX_IO_CALLS { return Err(AppError::IoCallLimit); }
        calls += 1;
        let end = capacity.min(filled.saturating_add(64 * 1024));
        match input.read(&mut bytes[filled..end]) {
            Ok(0) => break,
            Ok(count) if count <= end - filled => {
                filled += count;
                if filled > limit { return Err(AppError::InputLimit); }
            }
            Ok(_) => return Err(AppError::Io),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(_) => return Err(AppError::Io),
        }
    }
    let length = ByteLength::new(filled as u64);
    let visible = ByteRange::new(ByteOffset::new(0), ByteOffset::new(filled as u64)).map_err(|_| AppError::InvalidRange)?;
    let mut request = CaptureRequest::new(file_id(), revision()).map_err(|_| AppError::InvalidRange)?;
    if filled > 0 { request = request.with_range(visible).map_err(|_| AppError::InvalidRange)?; }
    let extent = ObservedExtent::from_bytes(request, length, &bytes[..filled], budget, allocation(11))?;
    if canceled() { return Err(AppError::Canceled); }
    Ok(Loaded { extent, visible, path: None, read_calls: calls as u64, _paths: paths })
}
