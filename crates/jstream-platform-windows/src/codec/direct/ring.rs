//! The direct encoders' input textures: NV12 textures of the encode size on the
//! context's D3D11 device, which encode targets are copied into on the GPU and which the
//! encoder then reads in place.

use jstream_platform::media::FrameSize;
use jstream_platform::{GpuFrame, PlatformError};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

use crate::com::OsResult;
use crate::gpu::EncodeGpu;

/// `count` NV12 textures of one size on the context's D3D11 device.
pub(crate) struct Ring {
    // The textures go before `gpu`, which keeps the device's driver loaded.
    textures: Vec<ID3D11Texture2D>,
    gpu: EncodeGpu,
}

impl Ring {
    /// Makes the textures, with the encode targets' bind flags, which the adapter is
    /// known to allocate in NV12.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the device refuses a texture.
    pub(crate) fn new(
        gpu: &EncodeGpu,
        size: FrameSize,
        count: usize,
    ) -> Result<Self, PlatformError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.width(),
            Height: size.height(),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut textures = Vec::with_capacity(count);
        for _ in 0..count {
            let mut texture = None;
            // SAFETY: plain COM call on the live device; the descriptor is valid (even
            // dimensions, checked by `video::check`) and the out-pointer a local.
            unsafe {
                gpu.device()
                    .CreateTexture2D(&desc, None, Some(&mut texture))
            }
            .os("ID3D11Device::CreateTexture2D (encoder input)")?;
            textures.push(texture.ok_or(PlatformError::Os {
                call: "ID3D11Device::CreateTexture2D (encoder input)",
                code: None,
                message: "returned no texture".into(),
            })?);
        }
        Ok(Self {
            textures,
            gpu: gpu.clone(),
        })
    }

    /// How many textures there are.
    pub(crate) fn len(&self) -> usize {
        self.textures.len()
    }

    /// The texture of `slot`.
    ///
    /// # Panics
    /// Never for a slot below [`len`](Self::len), the only ones `Slots` hands out.
    pub(crate) fn texture(&self, slot: usize) -> &ID3D11Texture2D {
        &self.textures[slot]
    }

    /// The context's GPU state.
    pub(crate) fn gpu(&self) -> &EncodeGpu {
        &self.gpu
    }

    /// Queues a copy of `frame`, an encode target of the context, into `slot`'s texture,
    /// and releases the target to its producer after it (`EncodeGpu::copy`).
    ///
    /// # Errors
    /// As for `EncodeGpu::copy`.
    pub(crate) fn copy(&self, frame: &GpuFrame, slot: usize) -> Result<(), PlatformError> {
        self.gpu.copy(frame, &self.textures[slot], 0)
    }
}
