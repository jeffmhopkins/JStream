//! Opening a wgpu device. The platform's [`GpuContext`](jstream_platform::GpuContext) is
//! preferred; this module is the path when interop is absent.

use std::future::Future;
use std::task::{Context, Poll, Waker};

use jstream_platform::GpuContext;

use crate::error::RenderError;

/// How [`crate::Compositor::open`] chooses an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    /// Try a software adapter first (lavapipe, WARP), then any adapter. When `false`,
    /// a hardware adapter first, then the software one.
    pub software: bool,
}

impl OpenOptions {
    /// Software adapter first, for a caller that wants CPU rasterisation even on a
    /// machine with a GPU. ARCH principle 7 needs only that the engine *can* run on one,
    /// which [`Self::hardware`]'s fallback already gives.
    pub const fn headless() -> Self {
        Self { software: true }
    }

    /// A hardware adapter first (the high-performance one when there are several),
    /// then the software adapter, so a machine with no GPU still runs. The default: a
    /// software adapter composites far slower than 30 fps needs (#433).
    pub const fn hardware() -> Self {
        Self { software: false }
    }
}

/// Hardware first: the engine accelerates everything it can, and the software adapter is
/// the fallback.
impl Default for OpenOptions {
    fn default() -> Self {
        Self::hardware()
    }
}

pub(crate) struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
    pub max_texture_dimension_2d: u32,
    pub max_buffer_size: u64,
}

impl Gpu {
    pub(crate) fn open(options: OpenOptions) -> Result<Self, RenderError> {
        // Debug builds would otherwise require the Khronos validation layer, which CI
        // does not install. `with_env` still honours `WGPU_VALIDATION` and the other
        // `WGPU_*` flags when a developer sets them.
        let instance = |backends: wgpu::Backends| {
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.flags = wgpu::InstanceFlags::empty().with_env();
            desc.backends = backends;
            wgpu::Instance::new(desc)
        };
        // On Windows a hardware adapter is D3D12 first: the platform's capture, interop
        // and encoders are D3D11/D3D12, and wgpu's own pick there was Vulkan (#433).
        if cfg!(windows)
            && !options.software
            && let Ok(adapter) = request(&instance(wgpu::Backends::DX12), false)
        {
            return Self::from_adapter(adapter);
        }
        let adapter = pick_adapter(&instance(wgpu::Backends::all()), options.software)?;
        Self::from_adapter(adapter)
    }

    pub(crate) fn from_context(ctx: &GpuContext) -> Self {
        let adapter = ctx.adapter().clone();
        let gpu = Self::bundle(adapter, ctx.device().clone(), ctx.queue().clone());
        gpu.log_open("platform context");
        gpu
    }

    fn from_adapter(adapter: wgpu::Adapter) -> Result<Self, RenderError> {
        let (device, queue) = ready(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("jstream-render"),
            ..Default::default()
        }))
        .ok_or_else(|| RenderError::Device {
            detail: "request_device did not finish on the native backend".into(),
        })?
        .map_err(|e| RenderError::Device {
            detail: e.to_string(),
        })?;
        let gpu = Self::bundle(adapter, device, queue);
        gpu.log_open("own device");
        Ok(gpu)
    }

    fn bundle(adapter: wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let limits = device.limits();
        Self {
            device,
            queue,
            adapter,
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
            max_buffer_size: limits.max_buffer_size,
        }
    }

    fn log_open(&self, source: &str) {
        let info = self.adapter.get_info();
        tracing::info!(
            source,
            name = %info.name,
            backend = ?info.backend,
            device_type = ?info.device_type,
            "opened compositor device"
        );
    }

    pub(crate) fn description(&self) -> String {
        let info = self.adapter.get_info();
        format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type)
    }
}

fn pick_adapter(instance: &wgpu::Instance, software: bool) -> Result<wgpu::Adapter, RenderError> {
    // The preferred kind first, then the other.
    let (first, second) = if software {
        (true, false)
    } else {
        (false, true)
    };
    match request(instance, first) {
        Ok(adapter) => Ok(adapter),
        Err(first_err) => request(instance, second).map_err(|second_err| RenderError::NoAdapter {
            detail: if software {
                format!("software adapter: {first_err}; any adapter: {second_err}")
            } else {
                format!("hardware adapter: {first_err}; software adapter: {second_err}")
            },
        }),
    }
}

fn request(instance: &wgpu::Instance, fallback: bool) -> Result<wgpu::Adapter, String> {
    let options = wgpu::RequestAdapterOptions {
        power_preference: if fallback {
            wgpu::PowerPreference::LowPower
        } else {
            wgpu::PowerPreference::HighPerformance
        },
        force_fallback_adapter: fallback,
        compatible_surface: None,
        apply_limit_buckets: false,
    };
    match ready(instance.request_adapter(&options)) {
        Some(Ok(adapter)) => Ok(adapter),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("the adapter request did not finish on the native backend".into()),
    }
}

/// Polls a future that native wgpu completes on the first poll. `None` when it doesn't.
fn ready<F: Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}
