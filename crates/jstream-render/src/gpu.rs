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
    /// then the software adapter, so a machine with no GPU still runs; a hardware
    /// adapter whose device won't open falls back to software as well. The default: on
    /// WARP a 720p program ran at about 10 fps (#433).
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
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.flags = wgpu::InstanceFlags::empty().with_env();
        let instance = wgpu::Instance::new(desc);
        let adapter = pick_adapter(&instance, options.software)?;
        // Without a GPU the hardware pick already returned the software adapter, and
        // asking for it again would only fail the same way.
        let software = adapter.get_info().device_type == wgpu::DeviceType::Cpu;
        let opened = Self::from_adapter(adapter);
        if options.software || software {
            return opened;
        }
        // A hardware adapter can be returned and still refuse its device (limits below
        // the defaults, a removed or hung GPU), so that falls back to software too.
        or_software(opened, || {
            let adapter = request(&instance, true).map_err(|detail| RenderError::NoAdapter {
                detail: format!("software adapter: {detail}"),
            })?;
            Self::from_adapter(adapter)
        })
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

/// `hardware`, or when it failed, what `software` opens. Both failures are reported
/// when both fail.
fn or_software<T>(
    hardware: Result<T, RenderError>,
    software: impl FnOnce() -> Result<T, RenderError>,
) -> Result<T, RenderError> {
    let hardware_err = match hardware {
        Ok(opened) => return Ok(opened),
        Err(err) => err,
    };
    tracing::warn!(
        error = %hardware_err,
        "the hardware adapter's device did not open; trying the software adapter"
    );
    software().map_err(|software_err| RenderError::Device {
        detail: format!("hardware: {hardware_err}; software: {software_err}"),
    })
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

#[cfg(test)]
mod tests {
    use super::or_software;
    use crate::RenderError;

    fn device(detail: &str) -> RenderError {
        RenderError::Device {
            detail: detail.into(),
        }
    }

    /// A hardware device that won't open falls back to the software one, and an error
    /// names both when neither opens (#437 B1).
    #[test]
    fn a_hardware_device_that_fails_falls_back_to_software() {
        assert!(matches!(or_software(Ok(1), || Ok(2)), Ok(1)));
        assert!(matches!(
            or_software(Err(device("limits")), || Ok(2)),
            Ok(2)
        ));
        let both = or_software::<u8>(Err(device("hung")), || Err(device("none")))
            .expect_err("neither opens");
        let text = both.to_string();
        assert!(text.contains("hung") && text.contains("none"), "{text}");
    }
}
