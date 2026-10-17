//! [`WindowsVcam`]: the producer side of the `jstream-vcam-proto` contract, and the
//! camera itself (`MFCreateVirtualCamera`).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use jstream_platform::{PlatformError, VcamFormat, VcamSink, VideoSurface};
use jstream_vcam_proto::{
    CLSID_STR, CONTROL_NAME, Control, FRIENDLY_NAME, HEIGHT, MAGIC, RING, STATE_EMPTY, STATE_LIVE,
    STATE_STOPPED, Scope, VERSION, WIDTH, fence_base, sddl_objects, slot_of, texture_base,
};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, FreeLibrary, GENERIC_ALL, HMODULE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext4, ID3D11Fence, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIDevice, IDXGIResource1,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFVirtualCamera, MF_VERSION, MFCreateVirtualCamera, MFSTARTUP_NOSOCKET,
    MFShutdown, MFStartup, MFVirtualCameraAccess_CurrentUser, MFVirtualCameraLifetime_Session,
    MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_ANY, RegGetValueW};
use windows::core::{HSTRING, Interface, s, w};

use super::format::{Pixels, check_format, contract_format, frame_pixels};
use super::shared::{Handle, Mapping, SecurityAttributes, qpc, session_id, user_sid};
use crate::com::{MtaUsage, OsResult};

/// How often the heartbeat thread marks the producer alive. Well inside the contract's
/// `LIVENESS_MS`, so a stall in the engine's frames shows the last frame, not the DLL's
/// colour bars.
const HEARTBEAT: Duration = Duration::from_millis(200);

/// The Windows virtual camera's engine side (`VcamSink`, ADR-0020).
///
/// [`start`](VcamSink::start) creates, in this session's namespace and with the
/// contract's DACL for this user, a ring of shared BGRA textures, a shared fence and the
/// control block, then creates the camera with `MFCreateVirtualCamera` for the current
/// user and this session. Frame Server then loads the `jstream-vcam-windows` media source,
/// which copies the frames [`publish`](VcamSink::publish) writes.
///
/// - **Format:** only the contract's, 1920x1080 BGRA at 30 fps
///   ([`offered_format`](Self::offered_format)). Frames are system-memory BGRA
///   (`VideoSurface::Cpu`); GPU frames are `Unsupported` for now.
/// - **Windows 11:** `start` first checks that `mfsensorgroup.dll` exports
///   `MFCreateVirtualCamera`, and is `Unsupported` without it, so a binary that
///   delay-loads the DLL gets an error rather than a delay-load exception.
/// - **Registration:** the media source must already be registered machine-wide (the
///   app's `--register-vcam` step). `start` checks for its `HKLM` class key and is
///   `Unsupported` without it.
/// - **One producer per session:** `start` is `InvalidState` while another live producer
///   (another engine, or another sink in this process) owns this session's control block.
/// - **Liveness:** a heartbeat thread keeps the block's heartbeat fresh while the sink is
///   started, so a pause in `publish` freezes the camera on its last frame. Stopping, or
///   the process ending, lets the DLL fall back to its colour bars.
/// - **Threads:** like the codecs, the sink keeps Media Foundation started and the process
///   MTA alive, so it may be driven from threads that never initialised COM. Start it from
///   a thread that isn't an STA.
#[derive(Default)]
pub struct WindowsVcam {
    live: Option<Live>,
}

impl WindowsVcam {
    /// A stopped sink. Nothing is opened until [`start`](VcamSink::start).
    pub fn new() -> Self {
        Self::default()
    }

    /// The one format the camera offers: 1920x1080 BGRA at 30 fps.
    ///
    /// # Errors
    /// None in practice; the contract's constants are valid sizes and rates.
    pub fn offered_format() -> Result<VcamFormat, PlatformError> {
        contract_format()
    }
}

impl std::fmt::Debug for WindowsVcam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsVcam")
            .field("format", &self.live.as_ref().map(|l| l.format))
            .field("frames", &self.live.as_ref().map(|l| l.producer.frame))
            .finish()
    }
}

impl VcamSink for WindowsVcam {
    fn start(&mut self, format: VcamFormat) -> Result<(), PlatformError> {
        if self.live.is_some() {
            return Err(PlatformError::InvalidState {
                what: "vcam start",
                state: "already started",
            });
        }
        check_format(&format)?;
        if !virtual_camera_api() {
            return Err(PlatformError::Unsupported {
                what: "the virtual camera: this Windows has no `MFCreateVirtualCamera` \
                       (it needs Windows 11)"
                    .into(),
            });
        }
        if !registered()? {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "the virtual camera: its media source {CLSID_STR} is not registered \
                     (registering it is optional: run `jstream-daemon --register-vcam`)"
                ),
            });
        }
        let mf = MfStarted::start()?;
        let producer = Producer::new()?;
        let heartbeat = Heartbeat::start(Arc::clone(&producer.mapping))?;
        let camera = Camera::create()?;
        self.live = Some(Live {
            camera,
            _heartbeat: heartbeat,
            producer,
            format,
            _mf: mf,
        });
        Ok(())
    }

    fn publish(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        let Some(live) = &mut self.live else {
            return Err(PlatformError::InvalidState {
                what: "vcam publish",
                state: "not started",
            });
        };
        let pixels = frame_pixels(&frame, &live.format)?;
        live.producer.publish(pixels)
    }

    fn stop(&mut self) -> Result<(), PlatformError> {
        let Some(live) = self.live.take() else {
            return Ok(());
        };
        // Everything else is released by the drop, whether or not the removal worked.
        live.camera.remove()
    }

    fn format(&self) -> Option<VcamFormat> {
        self.live.as_ref().map(|l| l.format)
    }
}

impl Drop for WindowsVcam {
    fn drop(&mut self) {
        // Dropping a started sink stops it (`VcamSink`); there is no one to report to.
        let _ = self.stop();
    }
}

/// A started camera. Fields drop in order: the camera goes first, so apps lose it before
/// the frames stop; then the heartbeat, then the producer's objects (marking the block
/// stopped), and Media Foundation last.
struct Live {
    camera: Camera,
    _heartbeat: Heartbeat,
    producer: Producer,
    format: VcamFormat,
    _mf: MfStarted,
}

/// Whether `mfsensorgroup.dll` loads from System32 and exports `MFCreateVirtualCamera`,
/// which Windows 11 added. A binary that delay-loads the DLL (the daemon does) would
/// otherwise raise a delay-load exception, not an error, on the first call into a
/// Windows 10. Only System32 is
/// searched, so a stray copy next to the exe can't satisfy it.
fn virtual_camera_api() -> bool {
    // SAFETY: plain call with a static, null-terminated name. Loading the DLL runs only
    // its own initialisation, which has no requirements on the caller.
    let Ok(module) =
        (unsafe { LoadLibraryExW(w!("mfsensorgroup.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) })
    else {
        return false;
    };
    // SAFETY: the module is loaded until the FreeLibrary below; the name is static and
    // null-terminated. The address is only tested, never called.
    let found = unsafe { GetProcAddress(module, s!("MFCreateVirtualCamera")) }.is_some();
    // SAFETY: the handle came from a successful LoadLibraryExW and is freed once. The
    // sink's own calls load the DLL again through the import or delay-load table.
    let _ = unsafe { FreeLibrary(module) };
    found
}

/// Whether the media source's class is registered machine-wide, where Frame Server looks
/// (ARCH › Packaging: it ignores `HKCU`).
fn registered() -> Result<bool, PlatformError> {
    let key = HSTRING::from(format!(
        r"Software\Classes\CLSID\{CLSID_STR}\InprocServer32"
    ));
    // SAFETY: plain call with a predefined root key and a valid wide string; no value is
    // read, so every out-pointer is absent.
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &key,
            windows::core::PCWSTR::null(),
            RRF_RT_ANY,
            None,
            None,
            None,
        )
    };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    err.ok().os("RegGetValueW")?;
    Ok(true)
}

/// Media Foundation started, and the process MTA kept alive, for as long as the sink is
/// started (as `MfRuntime` does for the codecs).
struct MfStarted {
    _mta: MtaUsage,
}

impl MfStarted {
    fn start() -> Result<Self, PlatformError> {
        let mta = MtaUsage::new()?;
        // SAFETY: plain call; balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }.os("MFStartup")?;
        Ok(Self { _mta: mta })
    }
}

impl Drop for MfStarted {
    fn drop(&mut self) {
        // SAFETY: balances the successful MFStartup in `start`; the MTA usage drops after.
        let _ = unsafe { MFShutdown() };
    }
}

/// The OS camera. Shut down on drop.
struct Camera(IMFVirtualCamera);

// SAFETY: the camera is created and used from MTA threads only (the sink keeps the
// process MTA alive and is started from a thread that isn't an STA), and the sink that
// owns it is driven by one thread at a time (`&mut self`).
unsafe impl Send for Camera {}

impl Camera {
    /// Creates the camera for the current user and this session, and starts it so apps
    /// see it.
    fn create() -> Result<Self, PlatformError> {
        // SAFETY: plain call with valid wide strings; no categories (the system default).
        let camera = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                &HSTRING::from(FRIENDLY_NAME),
                &HSTRING::from(CLSID_STR),
                None,
            )
        }
        .os("MFCreateVirtualCamera")?;
        // Wrapped first, so a failed start still shuts it down.
        let camera = Self(camera);
        // SAFETY: plain call on a live COM object; no callback means synchronous.
        unsafe { camera.0.Start(None::<&IMFAsyncCallback>) }.os("IMFVirtualCamera::Start")?;
        Ok(camera)
    }

    /// Removes the camera from the system. The drop still shuts it down.
    fn remove(&self) -> Result<(), PlatformError> {
        // SAFETY: plain call on a live COM object.
        unsafe { self.0.Remove() }.os("IMFVirtualCamera::Remove")
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        // SAFETY: plain call on a live COM object; errors at teardown change nothing.
        let _ = unsafe { self.0.Shutdown() };
    }
}

/// Keeps the control block's heartbeat fresh on its own thread. Stops and joins on drop.
struct Heartbeat {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Heartbeat {
    fn start(mapping: Arc<Mapping>) -> Result<Self, PlatformError> {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("jstream-vcam-heartbeat".into())
            .spawn(move || {
                // Beats until the sender is dropped (or sends).
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(HEARTBEAT) {
                    mapping
                        .control()
                        .heartbeat_qpc
                        .store(qpc(), Ordering::Release);
                }
            })
            .map_err(|e| PlatformError::Os {
                call: "std::thread::spawn",
                code: e.raw_os_error().map(i32::cast_unsigned),
                message: e.to_string(),
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            // The loop can't panic; a join error would change nothing here.
            let _ = thread.join();
        }
    }
}

/// The ring, the fence and the control block, on a D3D11 device of the sink's own.
///
/// Fields drop in order: the D3D objects, then the handles (and with them the names),
/// then the mapping. Dropping marks the block stopped first.
struct Producer {
    context: ID3D11DeviceContext4,
    textures: Vec<ID3D11Texture2D>,
    fence: ID3D11Fence,
    _device: ID3D11Device5,
    _handles: Vec<Handle>,
    _sa: SecurityAttributes,
    mapping: Arc<Mapping>,
    /// The last frame published, and the fence value signalled after it. 0 before any.
    frame: u64,
}

// SAFETY: the D3D11 device is created without `D3D11_CREATE_DEVICE_SINGLETHREADED`, so it
// and the textures and fence are free-threaded. The immediate context is not, but only
// `publish` uses it, through `&mut self` on the one thread driving the sink at a time.
// The mapping and handles are `Send` (see `shared`).
unsafe impl Send for Producer {}

impl Producer {
    fn new() -> Result<Self, PlatformError> {
        let (device, context) = create_device()?;
        let luid = adapter_luid(&device)?;
        let pid = std::process::id();
        let session = session_id()?;
        let sid = user_sid()?;
        let sddl = sddl_objects(&sid).ok_or_else(|| PlatformError::Unsupported {
            what: "a user SID the contract's DACL can't name".into(),
        })?;
        let sa = SecurityAttributes::from_sddl(&sddl)?;

        // A non-elevated engine can't create `Global\` objects (ARCH › Packaging), so the
        // hand-off is in this session's namespace, which the DLL opens as
        // `Session\<id>\…`.
        let scope = Scope::Session;
        let mapping = Mapping::create(&scope.create_name(CONTROL_NAME), &sa)?;
        let c = mapping.control();
        if mapping.existed && c.is_live() && c.heartbeat_fresh(qpc(), qpc_frequency()) {
            return Err(PlatformError::InvalidState {
                what: "vcam start",
                state: "another producer in this session is publishing to the camera",
            });
        }

        let mut handles = Vec::new();
        let mut textures = Vec::new();
        for slot in 0..RING {
            let name = scope.create_name(&texture_base(pid, slot));
            let (texture, handle) = shared_texture(&device, &name, &sa)?;
            textures.push(texture);
            handles.push(handle);
        }
        let mut fence: Option<ID3D11Fence> = None;
        // SAFETY: `fence` is a valid out-pointer.
        unsafe { device.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
            .os("ID3D11Device5::CreateFence")?;
        let fence = fence.ok_or_else(|| missing("ID3D11Device5::CreateFence"))?;
        // SAFETY: `sa` outlives the call; the name is a valid wide string.
        let handle = unsafe {
            fence.CreateSharedHandle(
                Some(sa.as_ptr()),
                GENERIC_ALL.0,
                &HSTRING::from(scope.create_name(&fence_base(pid))),
            )
        }
        .os("ID3D11Fence::CreateSharedHandle")?;
        handles.push(Handle(handle));

        publish_identity(c, scope, pid, session, luid);
        Ok(Self {
            context,
            textures,
            fence,
            _device: device,
            _handles: handles,
            _sa: sa,
            mapping: Arc::new(mapping),
            frame: 0,
        })
    }

    /// Writes the next frame into its ring slot, signals the fence with its number and
    /// publishes it ([`jstream_vcam_proto`]'s hand-off). Never waits on the consumer.
    fn publish(&mut self, pixels: Pixels<'_>) -> Result<(), PlatformError> {
        let n = self.frame + 1;
        let texture = usize::try_from(slot_of(n, RING))
            .ok()
            .and_then(|slot| self.textures.get(slot))
            .ok_or_else(|| missing("the ring slot"))?;
        let pitch = u32::try_from(pixels.stride).map_err(|_| PlatformError::Unsupported {
            what: "a virtual-camera frame with rows over 4 GiB apart".into(),
        })?;
        // SAFETY: `texture` is a live WIDTH x HEIGHT BGRA texture of this context's device,
        // and with no box the whole subresource is written. `frame_pixels` checked that
        // `pixels.data` holds HEIGHT rows of WIDTH * 4 bytes, `pitch` apart, so the read
        // stays inside it. D3D11 copies the data before returning.
        unsafe {
            self.context
                .UpdateSubresource(texture, 0, None, pixels.data.as_ptr().cast(), pitch, 0);
        }
        // SAFETY: plain calls on live objects of one device.
        unsafe { self.context.Signal(&self.fence, n) }.os("ID3D11DeviceContext4::Signal")?;
        // SAFETY: as above.
        unsafe { self.context.Flush() };
        self.frame = n;
        let c = self.mapping.control();
        let now = qpc();
        c.latest_qpc.store(now, Ordering::Release);
        c.latest_frame.store(n, Ordering::Release);
        c.heartbeat_qpc.store(now, Ordering::Release);
        Ok(())
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        // Before the objects go: the DLL drops its opened copies on seeing it.
        self.mapping
            .control()
            .state
            .store(STATE_STOPPED, Ordering::Release);
    }
}

/// Initialises the control block for this producer, as the contract says: marked empty
/// before the identity fields are rewritten (the DLL may hold an earlier producer's block
/// open), and live only once they are all written.
fn publish_identity(c: &Control, scope: Scope, pid: u32, session: u32, luid: u64) {
    c.state.store(STATE_EMPTY, Ordering::Release);
    c.scope.store(scope.as_u32(), Ordering::Release);
    c.producer_pid.store(pid, Ordering::Release);
    c.session_id.store(session, Ordering::Release);
    c.width.store(WIDTH, Ordering::Release);
    c.height.store(HEIGHT, Ordering::Release);
    c.dxgi_format.store(
        jstream_vcam_proto::DXGI_FORMAT_B8G8R8A8_UNORM,
        Ordering::Release,
    );
    c.ring.store(RING, Ordering::Release);
    c.adapter_luid.store(luid, Ordering::Release);
    c.latest_frame.store(0, Ordering::Release);
    c.latest_qpc.store(0, Ordering::Release);
    c.heartbeat_qpc.store(qpc(), Ordering::Release);
    c.magic.store(MAGIC, Ordering::Release);
    c.version.store(VERSION, Ordering::Release);
    c.state.store(STATE_LIVE, Ordering::Release);
}

/// A ring texture with a shared NT handle named `name`.
fn shared_texture(
    device: &ID3D11Device5,
    name: &str,
    sa: &SecurityAttributes,
) -> Result<(ID3D11Texture2D, Handle), PlatformError> {
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
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0).cast_unsigned(),
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
            .cast_unsigned(),
    };
    let mut texture = None;
    // SAFETY: `desc` is fully initialised and `texture` is a valid out-pointer.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .os("ID3D11Device::CreateTexture2D")?;
    let texture: ID3D11Texture2D =
        texture.ok_or_else(|| missing("ID3D11Device::CreateTexture2D"))?;
    let resource: IDXGIResource1 = texture.cast().os("IDXGIResource1")?;
    // SAFETY: `sa` outlives the call; the name is a valid wide string.
    let handle = unsafe {
        resource.CreateSharedHandle(
            Some(sa.as_ptr()),
            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
            &HSTRING::from(name),
        )
    }
    .os("IDXGIResource1::CreateSharedHandle")?;
    Ok((texture, Handle(handle)))
}

/// A D3D11 device on the default hardware adapter. The DLL opens the ring on whichever
/// adapter the control block names, so any adapter works.
fn create_device() -> Result<(ID3D11Device5, ID3D11DeviceContext4), PlatformError> {
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
    .os("D3D11CreateDevice")?;
    let device = device.ok_or_else(|| missing("D3D11CreateDevice"))?;
    let context = context.ok_or_else(|| missing("D3D11CreateDevice"))?;
    Ok((
        device.cast().os("ID3D11Device5")?,
        context.cast().os("ID3D11DeviceContext4")?,
    ))
}

/// The device's adapter LUID as the contract stores it: `high << 32 | low`.
fn adapter_luid(device: &ID3D11Device5) -> Result<u64, PlatformError> {
    let dxgi: IDXGIDevice = device.cast().os("IDXGIDevice")?;
    // SAFETY: plain calls on live COM objects.
    let desc =
        unsafe { dxgi.GetAdapter().and_then(|a| a.GetDesc()) }.os("IDXGIAdapter::GetDesc")?;
    Ok((u64::from(desc.AdapterLuid.HighPart.cast_unsigned()) << 32)
        | u64::from(desc.AdapterLuid.LowPart))
}

/// The QPC frequency, in ticks per second.
fn qpc_frequency() -> u64 {
    let mut f = 0i64;
    // SAFETY: `f` is a valid out-pointer; QPF can't fail on supported Windows.
    let _ = unsafe { windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f) };
    u64::try_from(f).unwrap_or(1)
}

/// A call that succeeded without returning its object.
fn missing(call: &'static str) -> PlatformError {
    PlatformError::Os {
        call,
        code: None,
        message: "succeeded but returned no object".into(),
    }
}

/// Platform tests on real hardware (R-TEST-004): behind `hw-tests` and `#[ignore]`d,
/// because CI machines have no Frame Server camera and no registered media source. Run
/// on Windows 11 with a D3D11 GPU, after registering the DLL (elevated):
/// `cargo test -p jstream-platform-windows --features hw-tests -- --ignored vcam`.
// Two attributes, so clippy sees a test module (`allow-unwrap-in-tests`).
#[cfg(test)]
#[cfg(feature = "hw-tests")]
mod hw_tests {
    use std::time::Instant;

    use jstream_platform::media::{ColorInfo, MediaTime, Timebase, VideoFrame, VideoFrameInfo};
    use windows::Win32::Media::MediaFoundation::{
        IMFSample, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM, MFCreateAttributes, MFCreateMediaType,
        MFCreateSourceReaderFromMediaSource, MFMediaType_Video, MFVideoFormat_RGB32,
    };

    use super::*;

    /// The test picture's colour, as B, G, R. A grey has no chroma, so it survives the
    /// DLL's BGRA to NV12 and the reader's NV12 to RGB32 whichever YUV matrix each uses;
    /// the DLL's colour bars are magenta at the centre.
    const COLOUR: [u8; 3] = [0x60, 0x60, 0x60];

    fn solid_frame(n: i64) -> VideoSurface {
        let format = WindowsVcam::offered_format().unwrap();
        let mut frame = VideoFrame::zeroed(VideoFrameInfo {
            size: format.size,
            format: format.pixel_format,
            color: ColorInfo::default(),
            pts: MediaTime::new(n, Timebase::per_second(30).unwrap()),
        })
        .unwrap();
        for y in 0..HEIGHT {
            for px in frame.row_mut(0, y).unwrap().chunks_exact_mut(4) {
                px.copy_from_slice(&[COLOUR[0], COLOUR[1], COLOUR[2], 0xff]);
            }
        }
        frame.into()
    }

    /// The B, G, R of the centre pixel of an RGB32 sample. The centre row is the same
    /// whichever way up the image is.
    fn centre(sample: &IMFSample) -> [u8; 3] {
        // SAFETY: plain calls on a live sample; the locked buffer is read within its
        // length and unlocked before it drops.
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().unwrap();
            let mut data = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut data, None, Some(&mut len)).unwrap();
            let bytes = std::slice::from_raw_parts(data, len as usize);
            let at = ((HEIGHT / 2 * WIDTH + WIDTH / 2) * 4) as usize;
            let px = [bytes[at], bytes[at + 1], bytes[at + 2]];
            buffer.Unlock().unwrap();
            px
        }
    }

    /// Whether a pixel is the test colour, give or take two YUV round trips' rounding.
    fn close(px: [u8; 3]) -> bool {
        px.iter().zip(COLOUR).all(|(a, b)| a.abs_diff(b) <= 6)
    }

    #[test]
    #[ignore = "needs Windows 11, a D3D11 GPU and the vcam DLL registered machine-wide"]
    fn frame_server_serves_the_published_frames() {
        let mut vcam = WindowsVcam::new();
        vcam.start(WindowsVcam::offered_format().unwrap()).unwrap();
        assert!(matches!(
            vcam.start(WindowsVcam::offered_format().unwrap()),
            Err(PlatformError::InvalidState { .. })
        ));
        // A second sink in this session finds the first one live.
        assert!(matches!(
            WindowsVcam::new().start(WindowsVcam::offered_format().unwrap()),
            Err(PlatformError::InvalidState { .. })
        ));
        let (source, mapping) = {
            let live = vcam.live.as_ref().unwrap();
            // SAFETY: plain call on a live COM object.
            let source = unsafe { live.camera.0.GetMediaSource() }.unwrap();
            (source, Arc::clone(&live.producer.mapping))
        };
        // Frame Server may offer only NV12 here, so the reader converts to RGB32 itself
        // (without video processing, asking for RGB32 is MF_E_TOPO_CODEC_NOT_FOUND).
        // SAFETY: plain MF calls on live objects; `attributes` is a valid out-pointer;
        // the stream index is the reader's first-video-stream constant.
        let reader = unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1).unwrap();
            let attributes = attributes.unwrap();
            attributes
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .unwrap();
            let reader = MFCreateSourceReaderFromMediaSource(&source, &attributes).unwrap();
            let rgb32 = MFCreateMediaType().unwrap();
            rgb32
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .unwrap();
            rgb32.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).unwrap();
            reader
                .SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &rgb32)
                .unwrap();
            reader
        };

        // The DLL retries opening the shared objects every so often, so give it time.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut seen = None;
        let mut n = 0;
        while Instant::now() < deadline {
            n += 1;
            vcam.publish(solid_frame(n)).unwrap();
            let (mut actual, mut flags, mut time) = (0u32, 0u32, 0i64);
            let mut sample = None;
            // SAFETY: plain call on a live reader; every out-pointer is a valid local
            // (synchronous ReadSample is E_POINTER without the flags).
            unsafe {
                reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        Some(&mut actual),
                        Some(&mut flags),
                        Some(&mut time),
                        Some(&mut sample),
                    )
                    .unwrap();
            }
            seen = sample.as_ref().map(centre);
            if seen.is_some_and(close) {
                break;
            }
            std::thread::sleep(Duration::from_millis(33));
        }
        let c = mapping.control();
        let shared = c.frames_shared.load(Ordering::Acquire);
        let hr = c.consumer_last_hr.load(Ordering::Acquire);
        assert!(
            seen.is_some_and(close),
            "centre pixel {seen:?} after {n} frames: shared {shared}, attaches {}, gpu opens \
             {}, last hr {hr:#010x}",
            c.consumer_attaches.load(Ordering::Acquire),
            c.gpu_opens.load(Ordering::Acquire),
        );
        assert!(shared > 0);

        drop(reader);
        // SAFETY: plain call on a live COM object.
        let _ = unsafe { source.Shutdown() };
        drop(source);
        vcam.stop().unwrap();
        assert_eq!(vcam.format(), None);
        assert_eq!(c.state.load(Ordering::Acquire), STATE_STOPPED);
        assert!(matches!(
            vcam.publish(solid_frame(n + 1)),
            Err(PlatformError::InvalidState { .. })
        ));
        // Stopped, the session's block is free for the next producer.
        vcam.start(WindowsVcam::offered_format().unwrap()).unwrap();
        vcam.stop().unwrap();
    }
}
