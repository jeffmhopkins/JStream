//! Where the stream's pixels come from: the producer's shared textures when a producer is
//! live, else the DLL's own test pattern.
//!
//! Consumer side of the hand-off in [`crate::proto`]: find the control block (`Global\`,
//! then the console session's and a few other sessions' namespaces), open the ring
//! textures and the fence by name on the producer's adapter, then per sample
//! `Wait(fence, n)`, `CopyResource` into a staging texture and map it. The CPU readback is
//! deliberate: the samples are system memory, which every Frame Server client accepts,
//! and the spike's question is the cross-process path, not zero-copy delivery.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GENERIC_ALL, HANDLE, HMODULE, LUID};
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

use super::log::log;
use super::shared::{Mapping, OwnedHandle, qpc, qpc_freq};
use crate::pixels::{bgra_to_nv12, copy_bgra, nv12_size, test_pattern};
use crate::proto::{CONTROL_NAME, Control, HEIGHT, RING, Scope, WIDTH, fence_base, texture_base};

const RETRY: Duration = Duration::from_secs(1);
/// A block whose producer has been silent this long is dropped and searched for again.
const STALE_LINK: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    Shared,
    Pattern,
}

pub(crate) struct FrameSource {
    bgra: Vec<u8>,
    served: u64,
    link: Option<Mapping>,
    link_last_live: Instant,
    next_link_try: Instant,
    gpu: Option<Gpu>,
    next_gpu_try: Instant,
    last_origin: Option<Origin>,
    last_error: Option<String>,
    qpc_freq: u64,
}

impl FrameSource {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            bgra: vec![0; WIDTH as usize * HEIGHT as usize * 4],
            served: 0,
            link: None,
            link_last_live: now,
            next_link_try: now,
            gpu: None,
            next_gpu_try: now,
            last_origin: None,
            last_error: None,
            qpc_freq: qpc_freq(),
        }
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
            let c = link.control();
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
        // SAFETY: plain call.
        let buffer = unsafe { MFCreateMemoryBuffer(size as u32) }?;
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
            buffer.SetCurrentLength(size as u32)?;
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
                    link.control()
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

    /// `Ok(true)` if `bgra` now holds the producer's newest frame.
    fn try_shared(&mut self) -> Result<bool> {
        let now = Instant::now();
        if self.link.is_none() && now >= self.next_link_try {
            self.next_link_try = now + RETRY;
            self.link = open_link();
            self.link_last_live = now;
        }
        let Some(link) = &self.link else {
            return Ok(false);
        };
        let c = link.control();
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
            let gpu = Gpu::open(c)?;
            c.gpu_opens.fetch_add(1, Ordering::AcqRel);
            self.gpu = Some(gpu);
        }
        let n = c.latest_frame.load(Ordering::Acquire);
        if n == 0 {
            return Ok(false);
        }
        if let Some(gpu) = &self.gpu {
            gpu.read(n, &mut self.bgra)?;
        }
        Ok(true)
    }
}

/// Session ids to try for a session-scoped control block: the console session first.
fn candidate_sessions() -> Vec<u32> {
    // SAFETY: plain call; returns 0xFFFFFFFF when no session is attached.
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    let mut ids = Vec::new();
    if console != u32::MAX {
        ids.push(console);
    }
    for id in 1..=4 {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

fn open_link() -> Option<Mapping> {
    let mut names = vec![Scope::Global.open_name(0, CONTROL_NAME)];
    names.extend(
        candidate_sessions()
            .into_iter()
            .map(|id| Scope::Session.open_name(id, CONTROL_NAME)),
    );
    let mut failures = Vec::new();
    for name in &names {
        match Mapping::open(name) {
            Ok(m) => {
                let c = m.control();
                c.consumer_pid.store(std::process::id(), Ordering::Release);
                c.consumer_attaches.fetch_add(1, Ordering::AcqRel);
                log(format_args!(
                    "frames: control block opened as {name} (after {} failures: {})",
                    failures.len(),
                    failures.join("; ")
                ));
                return Some(m);
            }
            Err(e) => failures.push(format!("{name}: {:#010x}", e.code().0)),
        }
    }
    log_once_per_minute(&format!(
        "frames: no control block (test pattern): {}",
        failures.join("; ")
    ));
    None
}

fn log_once_per_minute(msg: &str) {
    use std::sync::Mutex;
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = super::activate::lock(&LAST);
    if last.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
        *last = Some(Instant::now());
        log(msg);
    }
}

/// The producer's ring and fence, opened on this process's own D3D11 device.
struct Gpu {
    producer_pid: u32,
    context: ID3D11DeviceContext4,
    textures: Vec<ID3D11Texture2D>,
    fence: ID3D11Fence,
    staging: ID3D11Texture2D,
    ring: u32,
}

impl Gpu {
    fn open(c: &Control) -> Result<Self> {
        let pid = c.producer_pid.load(Ordering::Acquire);
        let session = c.session_id.load(Ordering::Acquire);
        let scope = Scope::from_u32(c.scope.load(Ordering::Acquire)).ok_or_else(|| {
            windows_core::Error::new(windows::Win32::Foundation::E_FAIL, "bad scope")
        })?;
        let ring = c.ring.load(Ordering::Acquire).clamp(1, RING);
        let (w, h) = (
            c.width.load(Ordering::Acquire),
            c.height.load(Ordering::Acquire),
        );
        let format = c.dxgi_format.load(Ordering::Acquire);
        if (w, h) != (WIDTH, HEIGHT) || format != DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32 {
            return Err(windows_core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                format!("producer frame {w}x{h} format {format} is not {WIDTH}x{HEIGHT} BGRA8"),
            ));
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
        // SAFETY: `d3d12` is a valid out-pointer. D3D12 is only used to open the fence's
        // name (D3D11 has no open-fence-by-name).
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut d3d12) }?;
        let d3d12 = d3d12.ok_or(windows::Win32::Foundation::E_POINTER)?;

        let mut textures = Vec::new();
        for slot in 0..ring {
            let name = scope.open_name(session, &texture_base(pid, slot));
            // SAFETY: plain call with a valid wide string; the texture holds its own
            // reference to the shared resource.
            let tex: ID3D11Texture2D = unsafe {
                device1.OpenSharedResourceByName(&HSTRING::from(&name), DXGI_SHARED_RESOURCE_READ.0)
            }
            .map_err(|e| annotate(e, &format!("OpenSharedResourceByName {name}")))?;
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
        let fence = fence.ok_or(windows::Win32::Foundation::E_POINTER)?;

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
        let staging = staging.ok_or(windows::Win32::Foundation::E_POINTER)?;

        log(format_args!(
            "frames: opened {ring} shared textures and the fence ({scope:?} scope, producer \
             pid {pid}, session {session}, adapter luid {luid_raw:#x})"
        ));
        Ok(Self {
            producer_pid: pid,
            context: context.cast()?,
            textures,
            fence,
            staging,
            ring,
        })
    }

    /// Copies frame `n` into `bgra` (tightly packed).
    fn read(&self, n: u64, bgra: &mut [u8]) -> Result<()> {
        let slot = (n % self.ring as u64) as usize;
        let tex = &self.textures[slot];
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: plain calls on live objects of the same device. The GPU waits for the
        // producer's signal of `n` before the copy; Map blocks until the copy is done.
        unsafe {
            self.context.Wait(&self.fence, n)?;
            self.context.CopyResource(&self.staging, tex);
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let (w, h) = (WIDTH as usize, HEIGHT as usize);
        let pitch = mapped.RowPitch as usize;
        if !mapped.pData.is_null() && pitch >= w * 4 {
            // SAFETY: a mapped staging texture is readable for RowPitch bytes per row for
            // `h` rows until Unmap; the slice ends at the last row's last pixel.
            let src = unsafe {
                std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (h - 1) + w * 4)
            };
            copy_bgra(src, pitch, bgra, w * 4, w, h);
        }
        // SAFETY: paired with the Map above.
        unsafe { self.context.Unmap(&self.staging, 0) };
        Ok(())
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
    Ok((
        device.ok_or(windows::Win32::Foundation::E_POINTER)?,
        context.ok_or(windows::Win32::Foundation::E_POINTER)?,
    ))
}

fn annotate(e: windows_core::Error, what: &str) -> windows_core::Error {
    windows_core::Error::new(e.code(), format!("{what}: {}", e.message()))
}
