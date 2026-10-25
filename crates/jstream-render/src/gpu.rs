//! Opening a wgpu device. The platform's [`GpuContext`](jstream_platform::GpuContext) is
//! preferred; this module is the path when interop is absent.

use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};

use jstream_platform::{ContextKey, GpuContext, SubmitLock};

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
    /// The platform context the device came from, or `None` for a device of its own.
    pub context: Option<ContextKey>,
    /// How the device has failed ([`watch`]).
    pub faults: Arc<Faults>,
    /// Held from staging a frame through the submit it arms: the context's, which
    /// platform threads that submit on the same queue take too, or one of its own.
    pub submit: SubmitLock,
}

/// What wgpu reported about a device outside any call's result.
#[derive(Default)]
pub(crate) struct Faults {
    /// Why the device was lost: set once, by wgpu's device-lost callback.
    pub lost: OnceLock<String>,
    /// The first uncaptured error since the compositor last took it.
    pub error: Mutex<Option<String>>,
}

/// Watches `device` for a loss and for uncaptured errors, instead of wgpu's default
/// handler, which panics on every uncaptured error. A device can be lost on any call
/// (a driver reset after a hang, Windows' TDR, a removed GPU), and every resource made
/// on it afterwards is invalid, so each later use is an uncaptured error. wgpu reports
/// the loss itself on a later poll, so the errors can come first, and a panic there
/// would end the render thread instead of letting it open a new device (#349). The
/// first error is kept for [`crate::Compositor::take_error`] and logged once until it is
/// taken, which `render` does each frame.
///
/// wgpu keeps one callback and one handler per device. The compositor is the only code
/// that sets them, on its own device and on the platform's context (the program stage
/// opens one compositor on it), so the context's loss is seen here too.
fn watch(device: &wgpu::Device) -> Arc<Faults> {
    let faults = Arc::new(Faults::default());
    let cell = Arc::clone(&faults);
    device.set_device_lost_callback(move |reason, message| {
        let why = match reason {
            wgpu::DeviceLostReason::Destroyed => "destroyed",
            wgpu::DeviceLostReason::Unknown => "lost by the driver",
        };
        let why = if message.is_empty() {
            why.to_owned()
        } else {
            format!("{why}: {message}")
        };
        tracing::warn!(reason = %why, "the compositor's GPU device was lost");
        let _ = cell.lost.set(why);
    });
    let cell = Arc::clone(&faults);
    device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
        let mut first = cell.error.lock().unwrap_or_else(PoisonError::into_inner);
        if first.is_none() {
            let error = error.to_string();
            if cell.lost.get().is_none() {
                tracing::error!(%error, "uncaptured wgpu error");
            }
            *first = Some(error);
        }
    }));
    faults
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
        // and encoders are D3D11/D3D12, and wgpu's own pick there was Vulkan (#433). A
        // D3D12 device that won't open goes on to the other backends, and so does WARP
        // (the D3D12 pick when there is no D3D12 GPU), so a GPU reachable only through
        // another backend still comes first.
        if cfg!(windows)
            && !options.software
            && let Ok(adapter) = request(&instance(wgpu::Backends::DX12), false)
            && adapter.get_info().device_type != wgpu::DeviceType::Cpu
        {
            match Self::from_adapter(adapter) {
                Ok(gpu) => return Ok(gpu),
                Err(error) => tracing::warn!(
                    %error,
                    "the D3D12 adapter's device did not open; trying the other backends"
                ),
            }
        }
        let instance = instance(wgpu::Backends::all());
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
        let mut gpu = Self::bundle(adapter, ctx.device().clone(), ctx.queue().clone());
        gpu.context = Some(ctx.key());
        gpu.submit = ctx.submit_lock().clone();
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
        let faults = watch(&device);
        Self {
            device,
            queue,
            adapter,
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
            max_buffer_size: limits.max_buffer_size,
            context: None,
            faults,
            submit: SubmitLock::new(),
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
