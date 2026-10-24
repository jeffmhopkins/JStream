//! Why a compositor call failed.

use jstream_platform::PlatformError;

/// Why opening a device, uploading an image or drawing a scene failed.
///
/// `Clone`, so a caller can keep the error after logging it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// The process has no wgpu adapter. Tests skip on this; CI treats it as a failure
    /// because the workflow installs a software rasterizer.
    #[error("no wgpu adapter: {detail}")]
    NoAdapter {
        /// What the backends reported.
        detail: String,
    },
    /// An adapter was found, but the device or a request that should finish inline did not.
    #[error("opening the wgpu device failed: {detail}")]
    Device {
        /// The backend's message.
        detail: String,
    },
    /// A placement, colour or opacity is non-finite or outside the range the compositor
    /// draws, an image size is zero, or an item's image was made by another compositor.
    #[error("invalid {what}: {detail}")]
    Invalid {
        /// Which value was refused (`placement`, `color`, `opacity`, `image`).
        what: &'static str,
        /// What is wrong with it.
        detail: String,
    },
    /// RGBA8 bytes do not match the image's dimensions.
    #[error("RGBA8 image is {width}x{height} ({expected} bytes) but {len} bytes were given")]
    ImageBytes {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Bytes provided.
        len: usize,
        /// `width * height * 4`, when that fits in `usize`.
        expected: usize,
    },
    /// A texture is not a single-sample 2D sRGB RGBA or BGRA image this compositor can sample.
    #[error("texture format {format:?} cannot be sampled as sRGB colour")]
    UnsupportedFormat {
        /// The texture's format.
        format: wgpu::TextureFormat,
    },
    /// The texture's format is sampled, but its shape or usage is not.
    #[error("the texture cannot be sampled: {detail}")]
    UnsupportedTexture {
        /// What about it was refused.
        detail: String,
    },
    /// A canvas or image is larger than the adapter's maximum 2D texture on one side.
    #[error("{what} {width}x{height} exceeds the adapter's maximum texture size of {limit}")]
    TooLarge {
        /// `canvas` or `image`.
        what: &'static str,
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// `max_texture_dimension_2d`.
        limit: u32,
    },
    /// More drawn items than a vertex buffer can address.
    #[error("the scene draws {count} items, which does not fit in one vertex buffer")]
    TooManyItems {
        /// Items that would have been drawn.
        count: usize,
    },
    /// Staging a [`jstream_platform::GpuFrame`](jstream_platform::GpuFrame) failed.
    #[error("staging a source frame failed: {0}")]
    Sync(PlatformError),
    /// wgpu raised an error no call returned, such as a validation error in a submitted
    /// command, or the invalid resources a lost device leaves
    /// ([`Compositor::take_error`](crate::Compositor::take_error)).
    #[error("the GPU reported an error: {detail}")]
    Uncaptured {
        /// wgpu's message.
        detail: String,
    },
    /// The GPU finished the frame, but mapping the readback buffer failed.
    #[error("reading the frame back failed: {detail}")]
    Readback {
        /// The map or copy error.
        detail: String,
    },
}

impl RenderError {
    /// Whether device creation found nothing to draw with.
    pub fn is_no_adapter(&self) -> bool {
        matches!(self, Self::NoAdapter { .. })
    }
}
