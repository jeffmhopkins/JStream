//! Where the stream's pixels come from: the producer's shared textures when a producer is
//! live, else the DLL's own test pattern.
//!
//! The consumer side of the [`jstream_vcam_proto`] hand-off: find the control block
//! ([`control_open_names`]: `Global\`, then the console session's and a few other sessions'
//! namespaces), open the ring textures and the fence by name on the producer's adapter,
//! then per sample pick a completed frame with [`choose_frame`], `Wait(fence, n)`,
//! `CopyResource` into a staging texture and map it. The samples are system memory, which
//! every Frame Server client accepts; a GPU-sample path (`SetD3DManager`) can come later.
//!
//! The block is written by another process, so nothing in it is trusted blindly: the
//! format must match the contract ([`Control::format_matches`]), the scope and session it
//! names must be the namespace the block was found in, and after the shared objects are
//! open the block must still be live and name the same producer.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_FAIL, E_POINTER, GENERIC_ALL, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext,
    ID3D11DeviceContext4, ID3D11Fence, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{D3D12CreateDevice, ID3D12Device};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_SHARED_RESOURCE_READ, IDXGIAdapter, IDXGIFactory4,
};
use windows::Win32::Media::MediaFoundation::{
    IMFSample, MFCreateMemoryBuffer, MFCreateSample, MFVideoFormat_NV12,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_core::{GUID, HSTRING, Interface, Result};

use jstream_vcam_proto::{
    CONTROL_NAME, Control, HEIGHT, RING, Scope, WIDTH, choose_frame, control_open_names,
    fence_base, slot_of, texture_base,
};

use super::log::{host_flags, log, session_id};
use super::shared::{Mapping, OwnedHandle, qpc, qpc_freq};
use crate::pixels::{bgra_to_nv12, copy_bgra, nv12_size, test_pattern};

/// How long to wait between attempts to find the block or open the shared objects.
const RETRY: Duration = Duration::from_secs(1);
/// A block whose producer has been silent this long is dropped and searched for again.
const STALE_LINK: Duration = Duration::from_secs(5);
/// How often "no control block" is logged while there is none.
const NO_LINK_LOG_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    Shared,
    Pattern,
}

/// The control block, and the name it was opened by.
struct Link {
    mapping: Mapping,
    name: String,
}

pub(crate) struct FrameSource {
    bgra: Vec<u8>,
    served: u64,
    link: Option<Link>,
    link_last_live: Instant,
    next_link_try: Instant,
    last_no_link_log: Option<Instant>,
    gpu: Option<Gpu>,
    next_gpu_try: Instant,
    last_origin: Option<Origin>,
    last_error: Option<String>,
    qpc_freq: u64,
}

impl FrameSource {
    /// Also tries to attach to the control block right away, so the producer sees the
    /// activation even before the first sample.
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        let mut frames = Self {
            bgra: vec![0; WIDTH as usize * HEIGHT as usize * 4],
            served: 0,
            link: None,
            link_last_live: now,
            next_link_try: now + RETRY,
            last_no_link_log: None,
            gpu: None,
            next_gpu_try: now,
            last_origin: None,
            last_error: None,
            qpc_freq: qpc_freq(),
        };
        frames.link = frames.open_link();
        if let Some(l) = &frames.link {
            l.mapping
                .control()
                .consumer_activations
                .fetch_add(1, Ordering::AcqRel);
        }
        frames
    }

    /// Releases the shared objects (stream shutdown).
    pub(crate) fn close(&mut self) {
        self.gpu = None;
        self.link = None;
    }

    /// One system-memory sample in `subtype` (NV12 or RGB32), without timestamps.
    pub(crate) fn make_sample(&mut self, subtype: &GUID) -> Result<IMFSample> {
        let origin = self.fill_bgra();
        self.served += 1;
        if self.last_origin != Some(origin) {
            log(format_args!(
                "frames: now serving {origin:?} (sample {})",
                self.served
            ));
            self.last_origin = Some(origin);
        }
        if self.served % 300 == 1 {
            log(format_args!("frames: {} samples served", self.served));
        }
        if let Some(link) = &self.link {
            let c = link.mapping.control();
            c.frames_served.fetch_add(1, Ordering::AcqRel);
            match origin {
                Origin::Shared => c.frames_shared.fetch_add(1, Ordering::AcqRel),
                Origin::Pattern => c.frames_pattern.fetch_add(1, Ordering::AcqRel),
            };
            c.consumer_heartbeat_qpc.store(qpc(), Ordering::Release);
        }

        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let nv12 = *subtype == MFVideoFormat_NV12;
        let size = if nv12 { nv12_size(w, h) } else { w * h * 4 };
        let size_u32 = u32::try_from(size).map_err(|_| E_FAIL)?;
        // SAFETY: plain call.
        let buffer = unsafe { MFCreateMemoryBuffer(size_u32) }?;
        let mut ptr = std::ptr::null_mut();
        let mut max = 0u32;
        // SAFETY: valid out-pointers; the buffer stays locked until Unlock below.
        unsafe { buffer.Lock(&mut ptr, Some(&mut max), None) }?;
        if !ptr.is_null() && max as usize >= size {
            // SAFETY: Lock returned `ptr` valid for `max >= size` writable bytes until
            // Unlock, and nothing else aliases it.
            let dst = unsafe { std::slice::from_raw_parts_mut(ptr, size) };
            if nv12 {
                bgra_to_nv12(&self.bgra, w * 4, dst, w, h);
            } else {
                copy_bgra(&self.bgra, w * 4, dst, w * 4, w, h);
            }
        }
        // SAFETY: paired with the Lock above.
        unsafe {
            buffer.Unlock()?;
            buffer.SetCurrentLength(size_u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            Ok(sample)
        }
    }

    fn fill_bgra(&mut self) -> Origin {
        match self.try_shared() {
            Ok(true) => Origin::Shared,
            Ok(false) => {
                test_pattern(&mut self.bgra, WIDTH as usize, HEIGHT as usize, self.served);
                Origin::Pattern
            }
            Err(e) => {
                let text = format!("{:#010x} {}", e.code().0, e.message());
                if self.last_error.as_deref() != Some(&text) {
                    log(format_args!("frames: shared path failed: {text}"));
                    self.last_error = Some(text);
                }
                if let Some(link) = &self.link {
                    link.mapping
                        .control()
                        .consumer_last_hr
                        .store(e.code().0, Ordering::Release);
                }
                self.gpu = None;
                self.next_gpu_try = Instant::now() + RETRY;
                test_pattern(&mut self.bgra, WIDTH as usize, HEIGHT as usize, self.served);
                Origin::Pattern
            }
        }
    }

    /// `Ok(true)` if `bgra` now holds one of the producer's frames.
    fn try_shared(&mut self) -> Result<bool> {
        let now = Instant::now();
        if self.link.is_none() && now >= self.next_link_try {
            self.next_link_try = now + RETRY;
            self.link = self.open_link();
            self.link_last_live = now;
        }
        let Some(link) = &self.link else {
            return Ok(false);
        };
        let c = link.mapping.control();
        if !c.is_live() || !c.heartbeat_fresh(qpc(), self.qpc_freq) {
            if now.duration_since(self.link_last_live) > STALE_LINK {
                log("frames: producer silent; dropping the control block");
                self.link = None;
                self.gpu = None;
            }
            return Ok(false);
        }
        self.link_last_live = now;

        let pid = c.producer_pid.load(Ordering::Acquire);
        if self.gpu.as_ref().is_some_and(|g| g.producer_pid != pid) {
            log("frames: new producer; reopening shared objects");
            self.gpu = None;
        }
        if self.gpu.is_none() {
            if now < self.next_gpu_try {
                return Ok(false);
            }
            self.next_gpu_try = now + RETRY;
            let gpu = Gpu::open(c, &link.name)?;
            // The producer may have restarted while the objects were being opened.
            if !c.is_live() || c.producer_pid.load(Ordering::Acquire) != gpu.producer_pid {
                log("frames: producer changed while opening its objects; retrying");
                return Ok(false);
            }
            c.gpu_opens.fetch_add(1, Ordering::AcqRel);
            self.gpu = Some(gpu);
        }
        let latest = c.latest_frame.load(Ordering::Acquire);
        match &self.gpu {
            Some(gpu) => gpu.read(latest, &mut self.bgra),
            None => Ok(false),
        }
    }

    /// Finds and attaches to a producer's control block, recording this consumer in it.
    fn open_link(&mut self) -> Option<Link> {
        // SAFETY: plain call; returns 0xFFFFFFFF when no session is attached.
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        let names = control_open_names((console != u32::MAX).then_some(console));
        let mut failures = Vec::new();
        for name in names {
            match Mapping::open(&name) {
                Ok(mapping) => {
                    let c = mapping.control();
                    c.consumer_pid.store(std::process::id(), Ordering::Release);
                    c.consumer_session_id
                        .store(session_id().unwrap_or(u32::MAX), Ordering::Release);
                    c.consumer_flags.store(host_flags(), Ordering::Release);
                    c.consumer_attaches.fetch_add(1, Ordering::AcqRel);
                    log(format_args!(
                        "frames: control block opened as {name} (after {} failures: {})",
                        failures.len(),
                        failures.join("; ")
                    ));
                    self.last_no_link_log = None;
                    return Some(Link { mapping, name });
                }
                Err(e) => failures.push(format!("{name}: {:#010x}", e.code().0)),
            }
        }
        let now = Instant::now();
        if self
            .last_no_link_log
            .is_none_or(|t| now.duration_since(t) > NO_LINK_LOG_EVERY)
        {
            self.last_no_link_log = Some(now);
            log(format_args!(
                "frames: no control block (test pattern): {}",
                failures.join("; ")
            ));
        }
        None
    }
}

/// The producer's ring and fence, opened on this process's own D3D11 device.
struct Gpu {
    producer_pid: u32,
    context: ID3D11DeviceContext4,
    textures: Vec<ID3D11Texture2D>,
    fence: ID3D11Fence,
    staging: ID3D11Texture2D,
}

impl Gpu {
    /// Opens the objects the live block `c`, found under `control_name`, points at.
    fn open(c: &Control, control_name: &str) -> Result<Self> {
        if !c.format_matches() {
            return Err(fail(format!(
                "producer format {}x{} dxgi {} ring {} is not the contract's {WIDTH}x{HEIGHT} \
                 BGRA ring {RING}",
                c.width.load(Ordering::Acquire),
                c.height.load(Ordering::Acquire),
                c.dxgi_format.load(Ordering::Acquire),
                c.ring.load(Ordering::Acquire),
            )));
        }
        let pid = c.producer_pid.load(Ordering::Acquire);
        let session = c.session_id.load(Ordering::Acquire);
        let scope = Scope::from_u32(c.scope.load(Ordering::Acquire))
            .ok_or_else(|| fail("the control block names no valid scope".into()))?;
        // The objects must live in the namespace the block itself was found in, so a block
        // can't point the DLL at another session's objects.
        if scope.open_name(session, CONTROL_NAME) != control_name {
            return Err(fail(format!(
                "the block found as {control_name} claims scope {} session {session}",
                scope.as_str()
            )));
        }
        let luid_raw = c.adapter_luid.load(Ordering::Acquire);
        let luid = LUID {
            LowPart: luid_raw as u32,
            HighPart: (luid_raw >> 32) as i32,
        };

        // SAFETY: plain call.
        let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory1() }?;
        // SAFETY: plain call on a live COM object.
        let adapter: IDXGIAdapter = unsafe { factory.EnumAdapterByLuid(luid) }?;
        let (device, context) = create_device(&adapter)?;
        let device1: ID3D11Device1 = device.cast()?;
        let device5: ID3D11Device5 = device.cast()?;
        let mut d3d12: Option<ID3D12Device> = None;
        // SAFETY: `d3d12` is a valid out-pointer. D3D12 is only used to open the fence by
        // name, which D3D11 can't do.
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut d3d12) }?;
        let d3d12 = d3d12.ok_or(E_POINTER)?;

        let mut textures = Vec::new();
        for slot in 0..RING {
            let name = scope.open_name(session, &texture_base(pid, slot));
            // SAFETY: plain call with a valid wide string; the texture holds its own
            // reference to the shared resource.
            let tex: ID3D11Texture2D = unsafe {
                device1.OpenSharedResourceByName(&HSTRING::from(&name), DXGI_SHARED_RESOURCE_READ.0)
            }
            .map_err(|e| annotate(e, &format!("OpenSharedResourceByName {name}")))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: `desc` is a valid out-pointer.
            unsafe { tex.GetDesc(&mut desc) };
            if (desc.Width, desc.Height, desc.Format) != (WIDTH, HEIGHT, DXGI_FORMAT_B8G8R8A8_UNORM)
            {
                return Err(fail(format!(
                    "{name} is {}x{} format {}, not {WIDTH}x{HEIGHT} BGRA",
                    desc.Width, desc.Height, desc.Format.0
                )));
            }
            textures.push(tex);
        }

        let fence_name = scope.open_name(session, &fence_base(pid));
        // SAFETY: plain call with a valid wide string; the handle is closed by OwnedHandle.
        let handle: HANDLE =
            unsafe { d3d12.OpenSharedHandleByName(&HSTRING::from(&fence_name), GENERIC_ALL.0) }
                .map_err(|e| annotate(e, &format!("OpenSharedHandleByName {fence_name}")))?;
        let handle = OwnedHandle(handle);
        let mut fence: Option<ID3D11Fence> = None;
        // SAFETY: `handle.0` is a live NT handle to a shared fence; `fence` is a valid
        // out-pointer. D3D11 takes its own reference, so the handle can close afterwards.
        unsafe { device5.OpenSharedFence(handle.0, &mut fence) }
            .map_err(|e| annotate(e, "ID3D11Device5::OpenSharedFence"))?;
        let fence = fence.ok_or(E_POINTER)?;

        let desc = D3D11_TEXTURE2D_DESC {
            Width: WIDTH,
            Height: HEIGHT,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        // SAFETY: `desc` is fully initialised; `staging` is a valid out-pointer.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }?;
        let staging = staging.ok_or(E_POINTER)?;

        log(format_args!(
            "frames: opened {RING} shared textures and the fence ({} scope, producer pid \
             {pid}, session {session}, adapter luid {luid_raw:#x})",
            scope.as_str()
        ));
        Ok(Self {
            producer_pid: pid,
            context: context.cast()?,
            textures,
            fence,
            staging,
        })
    }

    /// Copies a frame no newer than `latest` into `bgra` (tightly packed). `Ok(false)` if
    /// no completed frame is safe to read (see [`choose_frame`]).
    fn read(&self, latest: u64, bgra: &mut [u8]) -> Result<bool> {
        // SAFETY: plain call on a live COM object.
        let completed = unsafe { self.fence.GetCompletedValue() };
        let Some(n) = choose_frame(latest, completed, RING) else {
            return Ok(false);
        };
        let Some(tex) = self.textures.get(slot_of(n, RING) as usize) else {
            return Ok(false);
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: plain calls on live objects of the same device. The fence already
        // reached `n`, so the GPU wait is satisfied at once and never blocks on a producer
        // that died; Map blocks until the copy is done.
        unsafe {
            self.context.Wait(&self.fence, n)?;
            self.context.CopyResource(&self.staging, tex);
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let pitch = mapped.RowPitch as usize;
        let copied = !mapped.pData.is_null() && pitch >= w * 4 && {
            // SAFETY: a mapped staging texture is readable for RowPitch bytes per row for
            // `h` rows until Unmap; the slice ends at the last row's last pixel.
            let src = unsafe {
                std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (h - 1) + w * 4)
            };
            copy_bgra(src, pitch, bgra, w * 4, w, h)
        };
        // SAFETY: paired with the Map above.
        unsafe { self.context.Unmap(&self.staging, 0) };
        Ok(copied)
    }
}

fn create_device(adapter: &IDXGIAdapter) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: `adapter` is a live COM object; the out-pointers point to valid locals.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }?;
    Ok((device.ok_or(E_POINTER)?, context.ok_or(E_POINTER)?))
}

fn fail(msg: String) -> windows_core::Error {
    windows_core::Error::new(E_FAIL, msg)
}

fn annotate(e: windows_core::Error, what: &str) -> windows_core::Error {
    windows_core::Error::new(e.code(), format!("{what}: {}", e.message()))
}
