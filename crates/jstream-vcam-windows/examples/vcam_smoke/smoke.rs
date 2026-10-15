//! The Windows part of the smoke test.

use std::io::Write as _;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use jstream_vcam_proto::{
    CLSID_STR, CONTROL_NAME, Control, FPS, FRIENDLY_NAME, HEIGHT, MAGIC, MAPPING_SIZE, RING,
    STATE_EMPTY, STATE_LIVE, STATE_STOPPED, Scope, VERSION, WIDTH, fence_base, sddl_objects,
    slot_of, texture_base,
};
use windows::Win32::Foundation::{
    CloseHandle, GENERIC_ALL, HANDLE, HLOCAL, HMODULE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
    D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_IMMUTABLE, D3D11CreateDevice, ID3D11Device, ID3D11Device5, ID3D11DeviceContext,
    ID3D11DeviceContext4, ID3D11Fence, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIDevice, IDXGIResource1,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFVirtualCamera, MF_VERSION, MFCreateVirtualCamera, MFSTARTUP_FULL,
    MFShutdown, MFStartup, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_core::{HSTRING, Interface, PWSTR};

type Result<T> = std::result::Result<T, String>;

/// Adds what failed to a `windows` error.
trait Context<T> {
    fn ctx(self, what: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display> Context<T> for std::result::Result<T, E> {
    fn ctx(self, what: &str) -> Result<T> {
        self.map_err(|e| format!("{what}: {e}"))
    }
}

impl<T> Context<T> for Option<T> {
    fn ctx(self, what: &str) -> Result<T> {
        self.ok_or_else(|| format!("{what}: no value"))
    }
}

struct Args {
    seconds: u64,
    camera: bool,
    producer: bool,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        seconds: 60,
        camera: true,
        producer: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seconds" => {
                args.seconds = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ctx("--seconds needs a whole number")?;
            }
            "--no-camera" => args.camera = false,
            "--no-producer" => args.producer = false,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

macro_rules! say {
    ($($t:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

pub fn main() -> ExitCode {
    let result = parse_args().and_then(|args| {
        // SAFETY: called once on this thread before any COM use; "already initialised" is
        // fine.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // SAFETY: plain call; paired with MFShutdown below.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.ctx("MFStartup")?;
        let result = run(&args);
        // SAFETY: paired with MFStartup; every MF object was dropped in `run`.
        let _ = unsafe { MFShutdown() };
        result
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            say!("FAILED: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<()> {
    let producer = if args.producer {
        let p = Producer::new()?;
        say!(
            "producer: {WIDTH}x{HEIGHT} BGRA ring of {RING} on \"{}\", {} namespace, session {}",
            p.adapter,
            p.scope.as_str(),
            p.session_id
        );
        Some(p)
    } else {
        say!("producer: off (the camera should show the DLL's colour bars)");
        None
    };

    let camera = if args.camera {
        // SAFETY: plain call with valid wide strings; no categories (system default).
        let cam: IMFVirtualCamera = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &HSTRING::from(FRIENDLY_NAME),
                &HSTRING::from(CLSID_STR),
                None,
            )
        }
        .ctx("MFCreateVirtualCamera")?;
        // SAFETY: plain call on a live COM object; no callback means synchronous.
        unsafe { cam.Start(None::<&IMFAsyncCallback>) }.ctx("IMFVirtualCamera::Start")?;
        say!(
            "camera \"{FRIENDLY_NAME}\" started: open the Camera app and pick it. Dark teal with a \
             moving white bar is the producer's picture; colour bars mean the DLL runs without it."
        );
        Some(cam)
    } else {
        say!("camera: off");
        None
    };

    say!("running for {} s", args.seconds);
    let period = Duration::from_secs(1) / FPS;
    let start = Instant::now();
    let mut frame = 0u64;
    while start.elapsed() < Duration::from_secs(args.seconds) {
        frame += 1;
        if let Some(p) = &producer {
            p.render(frame)?;
        }
        let next = start + period * u32::try_from(frame).unwrap_or(u32::MAX);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }

    if let Some(cam) = &camera {
        // SAFETY: plain calls on a live COM object.
        unsafe {
            let _ = cam.Stop();
            let _ = cam.Remove();
        }
    }
    if let Some(p) = &producer {
        p.stop();
        report(p.control(), frame);
    }
    Ok(())
}

/// What the DLL wrote into the control block during the run.
fn report(c: &Control, frames: u64) {
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Acquire);
    say!("--- report (paste this back) ---");
    say!("producer frames: {frames}");
    say!(
        "consumer pid {} session {} flags {:#x} (1 session0, 2 svchost, 4 LOCAL SERVICE, 8 known)",
        c.consumer_pid.load(Ordering::Acquire),
        c.consumer_session_id.load(Ordering::Acquire),
        c.consumer_flags.load(Ordering::Acquire)
    );
    say!(
        "attaches {} activations {} gpu_opens {} last_hr {:#010x}",
        c.consumer_attaches.load(Ordering::Acquire),
        c.consumer_activations.load(Ordering::Acquire),
        c.gpu_opens.load(Ordering::Acquire),
        c.consumer_last_hr.load(Ordering::Acquire)
    );
    say!(
        "samples served {} shared {} pattern {}",
        get(&c.frames_served),
        get(&c.frames_shared),
        get(&c.frames_pattern)
    );
    // The stream is paced to the contract's rate, so this stays at or below about 1 while
    // a client watches for the whole run.
    let per_frame = get(&c.frames_served) as f64 / frames.max(1) as f64;
    say!("samples per producer frame: {per_frame:.2}");
    let ok = get(&c.frames_shared) > 0;
    say!(
        "verdict: {}",
        if ok {
            "PASS: Frame Server served the producer's frames"
        } else {
            "FAIL: no shared frames reached the camera"
        }
    );
}

/// The engine's side of the contract, as G3 will build it.
struct Producer {
    context: ID3D11DeviceContext4,
    textures: Vec<ID3D11Texture2D>,
    rtvs: Vec<ID3D11RenderTargetView>,
    bar: ID3D11Texture2D,
    fence: ID3D11Fence,
    /// The named NT handles: a name exists only while a handle to it is open.
    _handles: Vec<Handle>,
    mapping: Mapping,
    adapter: String,
    scope: Scope,
    session_id: u32,
}

const BAR_W: u32 = 64;
const BG: [f32; 4] = [0.0, 0.25, 0.3, 1.0];

impl Producer {
    fn new() -> Result<Self> {
        let (device, context) = create_device()?;
        let (adapter, luid) = adapter_facts(&device)?;
        let pid = std::process::id();
        let mut session_id = 0u32;
        // SAFETY: `session_id` is a valid out-pointer.
        unsafe { ProcessIdToSessionId(pid, &mut session_id) }.ctx("ProcessIdToSessionId")?;
        let sid = user_sid()?;
        let sddl = sddl_objects(&sid).ctx("the contract's DACL for this user")?;
        let sa = SecurityAttributes::from_sddl(&sddl)?;

        // A non-elevated user can't create `Global\` objects, so try it, then fall back to
        // this session's namespace, which the DLL opens as `Session\<id>\…`.
        let (scope, shared) = match Shared::create(&device, Scope::Global, pid, &sa) {
            Ok(s) => (Scope::Global, s),
            Err(_) => (
                Scope::Session,
                Shared::create(&device, Scope::Session, pid, &sa)?,
            ),
        };

        let c = shared.mapping.control();
        // The block may outlive an earlier run while the DLL holds it open: mark it empty
        // before rewriting the identity fields, as the contract says.
        c.state.store(STATE_EMPTY, Ordering::Release);
        c.scope.store(scope.as_u32(), Ordering::Release);
        c.producer_pid.store(pid, Ordering::Release);
        c.session_id.store(session_id, Ordering::Release);
        c.width.store(WIDTH, Ordering::Release);
        c.height.store(HEIGHT, Ordering::Release);
        c.dxgi_format
            .store(DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32, Ordering::Release);
        c.ring.store(RING, Ordering::Release);
        c.adapter_luid.store(luid, Ordering::Release);
        c.latest_frame.store(0, Ordering::Release);
        c.heartbeat_qpc.store(qpc(), Ordering::Release);
        c.magic.store(MAGIC, Ordering::Release);
        c.version.store(VERSION, Ordering::Release);
        c.state.store(STATE_LIVE, Ordering::Release);

        let mut rtvs = Vec::new();
        for tex in &shared.textures {
            let mut rtv = None;
            // SAFETY: `tex` is a live render-target texture; `rtv` is a valid out-pointer.
            unsafe { device.CreateRenderTargetView(tex, None, Some(&mut rtv)) }
                .ctx("CreateRenderTargetView")?;
            rtvs.push(rtv.ctx("CreateRenderTargetView")?);
        }
        Ok(Self {
            bar: white(&device, BAR_W, HEIGHT)?,
            context,
            textures: shared.textures,
            rtvs,
            fence: shared.fence,
            _handles: shared.handles,
            mapping: shared.mapping,
            adapter,
            scope,
            session_id,
        })
    }

    /// Renders frame `n` (1-based) into its ring slot, signals the fence with `n` and
    /// publishes it.
    fn render(&self, n: u64) -> Result<()> {
        let slot = slot_of(n, RING) as usize;
        let tex = &self.textures[slot];
        let bar_x = u32::try_from((n * 16) % u64::from(WIDTH - BAR_W)).unwrap_or(0);
        // SAFETY: plain calls on live objects of one device; the copy lies inside the
        // destination (bar_x + BAR_W <= WIDTH).
        unsafe {
            self.context.ClearRenderTargetView(&self.rtvs[slot], &BG);
            self.context
                .CopySubresourceRegion(tex, 0, bar_x, 0, 0, &self.bar, 0, None);
            self.context
                .Signal(&self.fence, n)
                .ctx("ID3D11DeviceContext4::Signal")?;
            self.context.Flush();
        }
        let c = self.mapping.control();
        let now = qpc();
        c.latest_qpc.store(now, Ordering::Release);
        c.latest_frame.store(n, Ordering::Release);
        c.heartbeat_qpc.store(now, Ordering::Release);
        Ok(())
    }

    fn control(&self) -> &Control {
        self.mapping.control()
    }

    fn stop(&self) {
        self.control().state.store(STATE_STOPPED, Ordering::Release);
    }
}

struct Shared {
    textures: Vec<ID3D11Texture2D>,
    fence: ID3D11Fence,
    handles: Vec<Handle>,
    mapping: Mapping,
}

impl Shared {
    fn create(
        device: &ID3D11Device5,
        scope: Scope,
        pid: u32,
        sa: &SecurityAttributes,
    ) -> Result<Self> {
        let mapping = Mapping::create(&scope.create_name(CONTROL_NAME), sa)?;
        let mut handles = Vec::new();
        let mut textures = Vec::new();
        for slot in 0..RING {
            let name = scope.create_name(&texture_base(pid, slot));
            let (tex, handle) = shared_texture(device, &name, sa)?;
            textures.push(tex);
            handles.push(handle);
        }
        let mut fence: Option<ID3D11Fence> = None;
        // SAFETY: `fence` is a valid out-pointer.
        unsafe { device.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
            .ctx("ID3D11Device5::CreateFence(SHARED)")?;
        let fence = fence.ctx("CreateFence")?;
        let fence_name = scope.create_name(&fence_base(pid));
        // SAFETY: `sa` outlives the call; the name is a valid wide string.
        let handle = unsafe {
            fence.CreateSharedHandle(
                Some(sa.as_ptr()),
                GENERIC_ALL.0,
                &HSTRING::from(&fence_name),
            )
        }
        .ctx(&format!("ID3D11Fence::CreateSharedHandle {fence_name}"))?;
        handles.push(Handle(handle));
        Ok(Self {
            textures,
            fence,
            handles,
            mapping,
        })
    }
}

fn shared_texture(
    device: &ID3D11Device5,
    name: &str,
    sa: &SecurityAttributes,
) -> Result<(ID3D11Texture2D, Handle)> {
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
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };
    let mut tex = None;
    // SAFETY: `desc` is fully initialised and `tex` is a valid out-pointer.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }
        .ctx("CreateTexture2D (shared)")?;
    let tex: ID3D11Texture2D = tex.ctx("CreateTexture2D")?;
    let res: IDXGIResource1 = tex.cast().ctx("IDXGIResource1")?;
    // SAFETY: `sa` outlives the call; the name is a valid wide string.
    let handle = unsafe {
        res.CreateSharedHandle(
            Some(sa.as_ptr()),
            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
            &HSTRING::from(name),
        )
    }
    .ctx(&format!("IDXGIResource1::CreateSharedHandle {name}"))?;
    Ok((tex, Handle(handle)))
}

/// An immutable white texture.
fn white(device: &ID3D11Device5, w: u32, h: u32) -> Result<ID3D11Texture2D> {
    let pixels = vec![255u8; (w * h * 4) as usize];
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr().cast(),
        SysMemPitch: w * 4,
        SysMemSlicePitch: 0,
    };
    let mut tex = None;
    // SAFETY: `init` points at `pixels`, which holds h rows of SysMemPitch bytes and
    // outlives the call (D3D11 copies it); `tex` is a valid out-pointer.
    unsafe { device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }
        .ctx("CreateTexture2D (bar)")?;
    tex.ctx("CreateTexture2D (bar)")
}

fn create_device() -> Result<(ID3D11Device5, ID3D11DeviceContext4)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: the out-pointers point to valid locals.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .ctx("D3D11CreateDevice")?;
    Ok((
        device.ctx("D3D11 device")?.cast().ctx("ID3D11Device5")?,
        context
            .ctx("D3D11 context")?
            .cast()
            .ctx("ID3D11DeviceContext4")?,
    ))
}

fn adapter_facts(device: &ID3D11Device5) -> Result<(String, u64)> {
    let dxgi: IDXGIDevice = device.cast().ctx("IDXGIDevice")?;
    // SAFETY: plain calls on live COM objects.
    let desc = unsafe { dxgi.GetAdapter().and_then(|a| a.GetDesc()) }.ctx("adapter")?;
    let len = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    let luid =
        (u64::from(desc.AdapterLuid.HighPart as u32) << 32) | u64::from(desc.AdapterLuid.LowPart);
    Ok((String::from_utf16_lossy(&desc.Description[..len]), luid))
}

/// This process's user SID as a string (`S-1-5-21-…`).
fn user_sid() -> Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: `token` is a valid out-pointer; the pseudo-handle needs no closing.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .ctx("OpenProcessToken")?;
    let token = Handle(token);
    let mut buf = [0u64; 64];
    let mut len = 0u32;
    // SAFETY: `buf` is writable for the size passed; `len` is a valid out-pointer.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            std::mem::size_of_val(&buf) as u32,
            &mut len,
        )
    }
    .ctx("GetTokenInformation(TokenUser)")?;
    let mut text = PWSTR::null();
    // SAFETY: the buffer starts with a TOKEN_USER (suitably aligned) whose SID points into
    // the same live buffer; `text` is a valid out-pointer, freed with LocalFree below.
    unsafe {
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        ConvertSidToStringSidW(user.User.Sid, &mut text)
    }
    .ctx("ConvertSidToStringSidW")?;
    // SAFETY: on success `text` is a NUL-terminated wide string.
    let sid = unsafe { text.to_string() }.ctx("SID text");
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc, freed once here.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    sid
}

/// A `SECURITY_ATTRIBUTES` built from an SDDL string. Frees the descriptor on drop.
struct SecurityAttributes {
    sd: PSECURITY_DESCRIPTOR,
    sa: SECURITY_ATTRIBUTES,
}

impl SecurityAttributes {
    fn from_sddl(sddl: &str) -> Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid wide string for the call; `sd` is a valid out-pointer,
        // freed with LocalFree on drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
        }
        .ctx("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
        Ok(Self {
            sd,
            sa: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            },
        })
    }

    fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.sa
    }
}

impl Drop for SecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: allocated with LocalAlloc by the SDDL conversion; freed exactly once.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.sd.0)));
        }
    }
}

/// Closes a handle on drop.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own the handle and close it exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// The control block's mapping, created by the producer.
struct Mapping {
    _handle: Handle,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl Mapping {
    fn create(name: &str, sa: &SecurityAttributes) -> Result<Self> {
        let size = u32::try_from(MAPPING_SIZE).ctx("mapping size")?;
        // SAFETY: `sa` outlives the call; the name is a valid wide string.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(sa.as_ptr()),
                PAGE_READWRITE,
                0,
                size,
                &HSTRING::from(name),
            )
        }
        .ctx(&format!("CreateFileMappingW {name}"))?;
        let handle = Handle(handle);
        // SAFETY: `handle` is a live section handle of MAPPING_SIZE bytes.
        let view = unsafe { MapViewOfFile(handle.0, FILE_MAP_ALL_ACCESS, 0, 0, MAPPING_SIZE) };
        if view.Value.is_null() {
            return Err(format!("MapViewOfFile {name} failed"));
        }
        Ok(Self {
            _handle: handle,
            view,
        })
    }

    fn control(&self) -> &Control {
        // SAFETY: the view is MAPPING_SIZE bytes, page-aligned and mapped until `self`
        // drops; `Control` is all atomics, so any bit pattern is valid.
        unsafe { &*(self.view.Value as *const Control) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmapped exactly once; no `control()` reference outlives `self`.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
        }
    }
}

fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: `v` is a valid out-pointer.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    u64::try_from(v).unwrap_or(0)
}
