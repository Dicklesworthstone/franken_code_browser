#![forbid(unsafe_code)]

//! Frozen raw-byte membership shared by saved-source search and navigation.
//! Scope admission uses the retained archive directory, never the filesystem.
//! Original ordinals remain stable for index lookup and exact hit activation.

use std::ops::Range;
use super::paged_snapshot::{PagedMemberData, PagedSearchError, SnapshotDirectory};

/// Only slash separates path components. No case folding, text conversion or
/// display-label comparison may broaden a directory or exact-member selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotScope<'a> { Archive, Directory(&'a [u8]), Member(&'a [u8]) }

impl<'a> SnapshotScope<'a> {
    pub fn validate(self) -> Result<(), PagedSearchError> {
        let path = match self {
            Self::Archive => return Ok(()),
            Self::Directory(path) | Self::Member(path) => path,
        };
        if path.len() > 16_384 || path.contains(&0)
            || path.split(|&byte| byte == b'/').any(|part| part.is_empty() || part == b"." || part == b"..") {
            return Err(PagedSearchError::InvalidScope);
        }
        Ok(())
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Archive => "whole-saved-archive",
            Self::Directory(_) => "saved-directory",
            Self::Member(_) => "saved-member",
        }
    }

    pub const fn path(self) -> &'a [u8] {
        match self { Self::Archive => b"", Self::Directory(path) | Self::Member(path) => path }
    }

    /// Select a contiguous interval in the raw-byte ordered directory. Count
    /// captured members using retained metadata only; no candidate bitmap,
    /// source loading, path cloning or index-page I/O is necessary.
    pub(crate) fn select(self, directory: &SnapshotDirectory) -> Result<SnapshotSelection, PagedSearchError> {
        self.validate()?;
        let members = match self {
            Self::Archive => 0..directory.len(),
            Self::Directory(prefix) => directory.descendants(prefix),
            Self::Member(path) => directory.find_path(path).map_or(0..0, |n| n..n + 1),
        };
        let captured = if self == Self::Archive { directory.captured_files() } else {
            members.clone().filter(|&ordinal| directory.member(ordinal)
                .is_some_and(|member| matches!(member.data, PagedMemberData::Captured { .. }))).count()
        };
        // A known exact member is a closed singleton even in an incomplete
        // archive. Missing membership must never become a successful miss.
        let complete = match self {
            Self::Member(_) => members.len() == 1,
            _ => directory.discovery_complete(),
        };
        Ok(SnapshotSelection { members, captured, complete })
    }
}

pub(crate) struct SnapshotSelection {
    pub members: Range<usize>,
    pub captured: usize,
    pub complete: bool,
}

impl SnapshotSelection {
    pub fn unavailable(&self) -> usize { self.members.len() - self.captured }
}
