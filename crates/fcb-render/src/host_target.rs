//! Host-target validation and drawable lifecycle ownership (FCB-089.B, fcb-npyi.2).
//!
//! Enforces the host lifecycle and rendering ownership contracts from plan §6.5 & §14.10:
//! - Native embedding host owns the event loop, window hierarchy, and display lifecycle.
//! - Exactly one owner acquires and presents each drawable; FCB never calls a second
//!   `nextDrawable` or presents the host's frame again.
//! - Target format, dimensions, sample count, color space, device identity, and completion
//!   ownership are validated before encoding.
//! - Incompatible target reuse (stale lease, device mismatch, presentation double-call)
//!   is rejected with typed errors.
//! - Wide-gamut/HDR (e.g. Display-P3) is an explicitly qualified optional route; SDR baseline
//!   is enforced unless wide-gamut capability is explicitly granted.

use fcb_core::ArenaOwnerId;
use std::fmt;

/// Errors arising from render-target descriptor validation or lease lifecycle violations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetValidationError {
    /// Target width or height is zero.
    ZeroDimension { width: u32, height: u32 },
    /// Target width or height exceeds maximum device limits (e.g. 16384).
    DimensionExceedsMaximum { width: u32, height: u32, max: u32 },
    /// Target device identity does not match the host's active device token.
    DeviceMismatch { expected_device: u64, actual_device: u64 },
    /// Target owner namespace does not match the active session owner.
    OwnerMismatch {
        expected_owner: ArenaOwnerId,
        actual_owner: ArenaOwnerId,
    },
    /// The target pixel format is unsupported or incompatible with the shader pipeline.
    UnsupportedFormat(TargetPixelFormat),
    /// The target sample count (MSAA) is unsupported.
    UnsupportedSampleCount(TargetSampleCount),
    /// Wide-gamut color space requested without explicit host capability enablement.
    WideGamutDisallowedWithoutCapability(TargetColorSpace),
    /// Attempted to encode into or reuse a target that is in an invalid lease state.
    IncompatibleTargetReuse { state: TargetLeaseState },
    /// Attempted to acquire a new drawable while another drawable lease is still active.
    MultipleActiveDrawables,
    /// Attempted to present a target that was not successfully encoded.
    TargetNotEncoded,
    /// The target lease has already been presented or released.
    AlreadyPresented,
}

impl fmt::Display for TargetValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimension { width, height } => {
                write!(f, "render target has zero dimension: {width}x{height}")
            }
            Self::DimensionExceedsMaximum { width, height, max } => {
                write!(
                    f,
                    "render target dimension {width}x{height} exceeds maximum {max}"
                )
            }
            Self::DeviceMismatch {
                expected_device,
                actual_device,
            } => {
                write!(
                    f,
                    "target device {actual_device} does not match expected device {expected_device}"
                )
            }
            Self::OwnerMismatch {
                expected_owner,
                actual_owner,
            } => {
                write!(
                    f,
                    "target owner {} does not match expected owner {}",
                    actual_owner.get(),
                    expected_owner.get()
                )
            }
            Self::UnsupportedFormat(fmt) => {
                write!(f, "unsupported target pixel format: {fmt:?}")
            }
            Self::UnsupportedSampleCount(samples) => {
                write!(f, "unsupported target sample count: {samples:?}")
            }
            Self::WideGamutDisallowedWithoutCapability(cs) => {
                write!(
                    f,
                    "wide-gamut color space {cs:?} disallowed without explicit capability grant"
                )
            }
            Self::IncompatibleTargetReuse { state } => {
                write!(
                    f,
                    "incompatible target reuse: target is in invalid lease state {state:?}"
                )
            }
            Self::MultipleActiveDrawables => {
                f.write_str("multiple active drawables acquired simultaneously")
            }
            Self::TargetNotEncoded => {
                f.write_str("cannot present target: rendering pass has not completed encoding")
            }
            Self::AlreadyPresented => {
                f.write_str("cannot present target: target has already been presented")
            }
        }
    }
}

impl std::error::Error for TargetValidationError {}

/// Supported target pixel formats.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TargetPixelFormat {
    /// 8-bit per channel BGRA, sRGB output encoding (standard macOS presentation format).
    Bgra8UnormSrgb,
    /// 8-bit per channel BGRA linear unorm.
    Bgra8Unorm,
    /// 8-bit per channel RGBA, sRGB output encoding.
    Rgba8UnormSrgb,
    /// 16-bit float RGBA (extended range / HDR route, requires explicit capability).
    Rgba16Float,
    /// Explicit unsupported format tag for negative testing.
    Unsupported(u32),
}

impl TargetPixelFormat {
    /// Returns true if this format is a valid SDR output target.
    pub const fn is_sdr_compatible(self) -> bool {
        matches!(
            self,
            Self::Bgra8UnormSrgb | Self::Bgra8Unorm | Self::Rgba8UnormSrgb
        )
    }

    /// Returns true if this format is wide-gamut / extended dynamic range.
    pub const fn is_wide_gamut(self) -> bool {
        matches!(self, Self::Rgba16Float)
    }
}

/// Target sample count (multisampling / MSAA).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TargetSampleCount {
    /// 1 sample per pixel (no MSAA).
    One,
    /// 4 samples per pixel (standard MSAA).
    Four,
    /// Unsupported sample count for boundary testing.
    Unsupported(u32),
}

impl TargetSampleCount {
    pub const fn count(self) -> u32 {
        match self {
            Self::One => 1,
            Self::Four => 4,
            Self::Unsupported(n) => n,
        }
    }

    pub const fn is_valid(self) -> bool {
        matches!(self, Self::One | Self::Four)
    }
}

/// Target color space encoding.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TargetColorSpace {
    /// Standard sRGB SDR baseline.
    Srgb,
    /// Display-P3 wide gamut (requires explicit capability grant).
    DisplayP3,
    /// Extended linear sRGB (requires explicit capability grant).
    ExtendedLinearSrgb,
}

impl TargetColorSpace {
    pub const fn is_sdr(self) -> bool {
        matches!(self, Self::Srgb)
    }

    pub const fn is_wide_gamut(self) -> bool {
        matches!(self, Self::DisplayP3 | Self::ExtendedLinearSrgb)
    }
}

/// Lifecycle state of an acquired render target lease.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TargetLeaseState {
    /// Drawable lease acquired by host, ready for command encoding.
    Acquired,
    /// Commands have been successfully encoded into the target pass.
    Encoded,
    /// Command buffer submitted to GPU completion queue.
    Submitted,
    /// Frame has been presented to the display; lease is terminal.
    Presented,
    /// Target lease revoked due to window occlusion, device loss, or resize.
    Revoked,
}

/// Maximum texture dimension supported by Apple Silicon Metal hardware.
pub const MAX_TARGET_DIMENSION: u32 = 16384;

/// Specification and metadata of a host-supplied render target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderTargetDescriptor {
    /// Host device identifier token.
    device_id: u64,
    /// Session arena owner namespace.
    owner_id: ArenaOwnerId,
    /// Drawable width in physical pixels.
    width: u32,
    /// Drawable height in physical pixels.
    height: u32,
    /// Pixel format of the backing texture.
    format: TargetPixelFormat,
    /// Sample count for rasterization.
    sample_count: TargetSampleCount,
    /// Color space declaration.
    color_space: TargetColorSpace,
    /// Monotonically increasing drawable generation.
    drawable_generation: u64,
    /// Current lifecycle state of this lease.
    state: TargetLeaseState,
}

impl RenderTargetDescriptor {
    pub fn new(
        device_id: u64,
        owner_id: ArenaOwnerId,
        width: u32,
        height: u32,
        format: TargetPixelFormat,
        sample_count: TargetSampleCount,
        color_space: TargetColorSpace,
        drawable_generation: u64,
    ) -> Self {
        Self {
            device_id,
            owner_id,
            width,
            height,
            format,
            sample_count,
            color_space,
            drawable_generation,
            state: TargetLeaseState::Acquired,
        }
    }

    pub const fn device_id(&self) -> u64 {
        self.device_id
    }

    pub const fn owner_id(&self) -> ArenaOwnerId {
        self.owner_id
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn format(&self) -> TargetPixelFormat {
        self.format
    }

    pub const fn sample_count(&self) -> TargetSampleCount {
        self.sample_count
    }

    pub const fn color_space(&self) -> TargetColorSpace {
        self.color_space
    }

    pub const fn drawable_generation(&self) -> u64 {
        self.drawable_generation
    }

    pub const fn state(&self) -> TargetLeaseState {
        self.state
    }

    /// Validate this target descriptor against host expectations and device limits.
    pub fn validate(
        &self,
        expected_device: u64,
        expected_owner: ArenaOwnerId,
        allow_wide_gamut: bool,
    ) -> Result<(), TargetValidationError> {
        if self.width == 0 || self.height == 0 {
            return Err(TargetValidationError::ZeroDimension {
                width: self.width,
                height: self.height,
            });
        }
        if self.width > MAX_TARGET_DIMENSION || self.height > MAX_TARGET_DIMENSION {
            return Err(TargetValidationError::DimensionExceedsMaximum {
                width: self.width,
                height: self.height,
                max: MAX_TARGET_DIMENSION,
            });
        }
        if self.device_id != expected_device {
            return Err(TargetValidationError::DeviceMismatch {
                expected_device,
                actual_device: self.device_id,
            });
        }
        if self.owner_id != expected_owner {
            return Err(TargetValidationError::OwnerMismatch {
                expected_owner,
                actual_owner: self.owner_id,
            });
        }
        if matches!(self.format, TargetPixelFormat::Unsupported(_)) {
            return Err(TargetValidationError::UnsupportedFormat(self.format));
        }
        if !self.sample_count.is_valid() {
            return Err(TargetValidationError::UnsupportedSampleCount(
                self.sample_count,
            ));
        }
        if self.color_space.is_wide_gamut() && !allow_wide_gamut {
            return Err(TargetValidationError::WideGamutDisallowedWithoutCapability(
                self.color_space,
            ));
        }
        if self.format.is_wide_gamut() && !allow_wide_gamut {
            return Err(TargetValidationError::WideGamutDisallowedWithoutCapability(
                self.color_space,
            ));
        }
        if self.state != TargetLeaseState::Acquired {
            return Err(TargetValidationError::IncompatibleTargetReuse {
                state: self.state,
            });
        }
        Ok(())
    }

    /// Mark the target as having successfully encoded commands.
    pub fn mark_encoded(&mut self) -> Result<(), TargetValidationError> {
        if self.state != TargetLeaseState::Acquired {
            return Err(TargetValidationError::IncompatibleTargetReuse {
                state: self.state,
            });
        }
        self.state = TargetLeaseState::Encoded;
        Ok(())
    }

    /// Mark the target as submitted for GPU execution.
    pub fn mark_submitted(&mut self) -> Result<(), TargetValidationError> {
        if self.state != TargetLeaseState::Encoded {
            return Err(TargetValidationError::IncompatibleTargetReuse {
                state: self.state,
            });
        }
        self.state = TargetLeaseState::Submitted;
        Ok(())
    }

    /// Mark the target as presented to the display.
    pub fn mark_presented(&mut self) -> Result<(), TargetValidationError> {
        if self.state == TargetLeaseState::Presented {
            return Err(TargetValidationError::AlreadyPresented);
        }
        if self.state != TargetLeaseState::Submitted {
            return Err(TargetValidationError::TargetNotEncoded);
        }
        self.state = TargetLeaseState::Presented;
        Ok(())
    }

    /// Revoke the target lease.
    pub fn revoke(&mut self) {
        self.state = TargetLeaseState::Revoked;
    }
}

/// A safe host-managed drawable lease tracker ensuring exactly-one active drawable.
#[derive(Debug)]
pub struct HostDrawableLeaseTracker {
    device_id: u64,
    owner_id: ArenaOwnerId,
    allow_wide_gamut: bool,
    active_generation: Option<u64>,
    active_lease: Option<RenderTargetDescriptor>,
    total_presented: u64,
}

impl HostDrawableLeaseTracker {
    pub fn new(device_id: u64, owner_id: ArenaOwnerId, allow_wide_gamut: bool) -> Self {
        Self {
            device_id,
            owner_id,
            allow_wide_gamut,
            active_generation: None,
            active_lease: None,
            total_presented: 0,
        }
    }

    pub const fn active_lease(&self) -> Option<&RenderTargetDescriptor> {
        self.active_lease.as_ref()
    }

    pub const fn total_presented(&self) -> u64 {
        self.total_presented
    }

    /// Acquire a new drawable lease from the host.
    pub fn acquire_drawable(
        &mut self,
        target: RenderTargetDescriptor,
    ) -> Result<&RenderTargetDescriptor, TargetValidationError> {
        if self.active_lease.is_some() {
            return Err(TargetValidationError::MultipleActiveDrawables);
        }
        target.validate(self.device_id, self.owner_id, self.allow_wide_gamut)?;
        self.active_generation = Some(target.drawable_generation());
        self.active_lease = Some(target);
        Ok(self.active_lease.as_ref().expect("just set"))
    }

    /// Complete encoding into the active lease.
    pub fn encode_pass(&mut self) -> Result<(), TargetValidationError> {
        let lease = self
            .active_lease
            .as_mut()
            .ok_or(TargetValidationError::IncompatibleTargetReuse {
                state: TargetLeaseState::Revoked,
            })?;
        lease.mark_encoded()
    }

    /// Submit the active lease to the GPU queue.
    pub fn submit_pass(&mut self) -> Result<(), TargetValidationError> {
        let lease = self
            .active_lease
            .as_mut()
            .ok_or(TargetValidationError::IncompatibleTargetReuse {
                state: TargetLeaseState::Revoked,
            })?;
        lease.mark_submitted()
    }

    /// Present the active drawable and release the lease.
    pub fn present_drawable(&mut self) -> Result<u64, TargetValidationError> {
        let mut lease = self
            .active_lease
            .take()
            .ok_or(TargetValidationError::TargetNotEncoded)?;
        lease.mark_presented()?;
        self.active_generation = None;
        self.total_presented += 1;
        Ok(self.total_presented)
    }

    /// Revoke the active lease on window teardown or device loss.
    pub fn revoke_active(&mut self) {
        if let Some(mut lease) = self.active_lease.take() {
            lease.revoke();
        }
        self.active_generation = None;
    }
}
