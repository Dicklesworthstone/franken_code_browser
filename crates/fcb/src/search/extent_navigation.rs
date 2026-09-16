#![forbid(unsafe_code)]

//! Path/atlas activation for files that have NOT been captured in full.
//! A host performs its authorized positioned read on a worker, then delivers
//! the observed extent under the original navigation request. Neither route
//! resolves a display label, reopens a provider, or assumes unread bytes exist.

use crate::{BrowserSession, FcbError};
use super::{NativeSourceIdentity, PathNavigationTarget, QueryGeneration, SearchManifestId};
use super::extents::{ExtentView, ExtentViewError, ObservedExtent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExtentActivationError {
    Source(FcbError),
    View(ExtentViewError),
    #[cfg(feature = "map")]
    Atlas(crate::map::AtlasNavigationError),
}
impl std::fmt::Display for ExtentActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => write!(f, "{error}"),
            Self::View(error) => write!(f, "{error}"),
            #[cfg(feature = "map")]
            Self::Atlas(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for ExtentActivationError {}
impl From<FcbError> for ExtentActivationError { fn from(error: FcbError) -> Self { Self::Source(error) } }
impl From<ExtentViewError> for ExtentActivationError { fn from(error: ExtentViewError) -> Self { Self::View(error) } }
#[cfg(feature = "map")]
impl From<crate::map::AtlasNavigationError> for ExtentActivationError {
    fn from(error: crate::map::AtlasNavigationError) -> Self { Self::Atlas(error) }
}

impl BrowserSession {
    pub fn open_path_extent(&self, target: &PathNavigationTarget<'_>, active_index: SearchManifestId,
        active_generation: QueryGeneration, native: NativeSourceIdentity<'_>, extent: ObservedExtent)
        -> Result<ExtentView, ExtentActivationError> {
        target.validate_delivery(active_index, active_generation)?;
        if target.file().owner() != self.owner() || native.root.owner() != self.owner()
            || native.root != target.root() { return Err(FcbError::OwnerMismatch.into()); }
        if extent.request().file() != target.file() { return Err(FcbError::SourceNotFound.into()); }
        if native.path != target.native_path() { return Err(FcbError::StaleGeneration.into()); }
        Ok(self.open_extent(extent)?)
    }

    #[cfg(feature = "map")]
    pub fn open_atlas_extent(&self, sources: &crate::map::AtlasSources<'_, '_>,
        presented: crate::map::PresentedAtlas<'_>, target: crate::map::AtlasSourceTarget,
        extent: ObservedExtent) -> Result<ExtentView, ExtentActivationError> {
        target.validate(sources, presented)?;
        if extent.request().file() != target.file() {
            return Err(crate::map::AtlasNavigationError::WrongCapture.into());
        }
        Ok(self.open_extent(extent)?)
    }
}
