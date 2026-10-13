//! The producer side: a D3D11 device, the shared ring textures, fence and control block,
//! and the test-pattern renderer (plain clears and copies, no shaders).
//!
//! Picture: dark teal, a white vertical bar moving right, and a row of 32 squares
//! showing the frame counter in binary (most significant bit on the left). The DLL's own
//! fallback is colour bars, so a viewer can tell the two apart.

use std::sync::atomic::Ordering;

use s2_vcam::proto::{
    CONTROL_NAME, HEIGHT, MAGIC, RING, STATE_LIVE, STATE_STOPPED, Scope, VERSION, WIDTH,
    fence_base, texture_base,
};
use s2_vcam::win::shared::{Mapping, OwnedHandle, SDDL_OBJECTS, SecurityAttributes, qpc};
use windows::Win32::Foundation::{GENERIC_ALL, HMODULE};
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
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_core::{HSTRING, Interface};

use crate::err::{Context, Result};

const BAR_W: u32 = 64;
const BIT: u32 = 40;
const BIT_GAP: u32 = 8;
const BG: [f32; 4] = [0.0, 0.25, 0.3, 1.0];

/// Facts about the producer's setup, for the report.
pub struct Setup {
    pub scope: Scope,
    pub global_error: Option<String>,
    pub control_name: String,
    pub control_existed: bool,
    pub session_id: u32,
    pub adapter: String,
    pub adapter_luid: u64,
}

pub struct Producer {
    context: ID3D11DeviceContext4,
    textures: Vec<ID3D11Texture2D>,
    rtvs: Vec<ID3D11RenderTargetView>,
    bar: ID3D11Texture2D,
    bit_on: ID3D11Texture2D,
    bit_off: ID3D11Texture2D,
    fence: ID3D11Fence,
    /// The named NT handles; the names exist only while a handle is open.
    _handles: Vec<OwnedHandle>,
    mapping: Mapping,
    pub setup: Setup,
}

impl Producer {
    pub fn new() -> Result<Self> {
        let (device, context) = create_device()?;
        let (adapter, adapter_luid) = adapter_facts(&device)?;
        let pid = std::process::id();
        let mut session_id = 0u32;
        // SAFETY: `session_id` is a valid out-pointer.
        unsafe { ProcessIdToSessionId(pid, &mut session_id) }.context("ProcessIdToSessionId")?;
        let sa = SecurityAttributes::from_sddl(SDDL_OBJECTS).context("SDDL for shared objects")?;

        // Prefer the global namespace; a non-elevated user usually can't create a global
        // file mapping (SeCreateGlobalPrivilege), so fall back to the session namespace.
        let (scope, global_error, shared) =
            match SharedObjects::create(&device, Scope::Global, pid, &sa) {
                Ok(s) => (Scope::Global, None, s),
                Err(e) => {
                    let s = SharedObjects::create(&device, Scope::Session, pid, &sa)
                        .context("creating the shared objects in the session namespace")?;
                    (Scope::Session, Some(e.to_string()), s)
                }
            };

        let c = shared.mapping.control();
        // The block may persist from a previous run (Frame Server holds it open), so
        // reset the producer fields; the consumer's counters keep accumulating.
        c.state.store(0, Ordering::Release);
        c.scope.store(scope.as_u32(), Ordering::Release);
        c.producer_pid.store(pid, Ordering::Release);
        c.session_id.store(session_id, Ordering::Release);
        c.width.store(WIDTH, Ordering::Release);
        c.height.store(HEIGHT, Ordering::Release);
        c.dxgi_format
            .store(DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32, Ordering::Release);
        c.ring.store(RING, Ordering::Release);
        c.adapter_luid.store(adapter_luid, Ordering::Release);
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
                .context("CreateRenderTargetView")?;
            rtvs.push(rtv.context("no RTV")?);
        }

        let setup = Setup {
            scope,
            global_error,
            control_name: shared.mapping.name.clone(),
            control_existed: shared.mapping.existed,
            session_id,
            adapter,
            adapter_luid,
        };
        Ok(Self {
            bar: solid(&device, BAR_W, HEIGHT, [255, 255, 255, 255])?,
            bit_on: solid(&device, BIT, BIT, [255, 255, 255, 255])?,
            bit_off: solid(&device, BIT, BIT, [60, 60, 60, 255])?,
            context,
            textures: shared.textures,
            rtvs,
            fence: shared.fence,
            _handles: shared.handles,
            mapping: shared.mapping,
            setup,
        })
    }

    /// Renders frame `n` (1-based) into ring slot `n % RING`, signals the fence with `n`
    /// and publishes it.
    pub fn render(&self, n: u64) -> Result<()> {
        let slot = (n % RING as u64) as usize;
        let tex = &self.textures[slot];
        let bar_x = ((n * 16) % u64::from(WIDTH - BAR_W)) as u32;
        // SAFETY: plain calls on live objects of one device; the copy rectangles lie
        // inside the destination (bar_x + BAR_W <= WIDTH; 32 squares fit in 1920 px).
        unsafe {
            self.context.ClearRenderTargetView(&self.rtvs[slot], &BG);
            self.context
                .CopySubresourceRegion(tex, 0, bar_x, 0, 0, &self.bar, 0, None);
            for bit in 0..32u32 {
                let x = 40 + (31 - bit) * (BIT + BIT_GAP);
                let src = if (n >> bit) & 1 == 1 {
                    &self.bit_on
                } else {
                    &self.bit_off
                };
                self.context
                    .CopySubresourceRegion(tex, 0, x, 40, 0, src, 0, None);
            }
            self.context
                .Signal(&self.fence, n)
                .context("ID3D11DeviceContext4::Signal")?;
            self.context.Flush();
        }
        let c = self.mapping.control();
        let now = qpc();
        c.latest_qpc.store(now, Ordering::Release);
        c.latest_frame.store(n, Ordering::Release);
        c.heartbeat_qpc.store(now, Ordering::Release);
        Ok(())
    }

    pub fn control(&self) -> &s2_vcam::proto::Control {
        self.mapping.control()
    }

    pub fn stop(&self) {
        self.mapping
            .control()
            .state
            .store(STATE_STOPPED, Ordering::Release);
    }
}

struct SharedObjects {
    textures: Vec<ID3D11Texture2D>,
    fence: ID3D11Fence,
    handles: Vec<OwnedHandle>,
    mapping: Mapping,
}

impl SharedObjects {
    fn create(
        device: &ID3D11Device5,
        scope: Scope,
        pid: u32,
        sa: &SecurityAttributes,
    ) -> Result<Self> {
        let control_name = scope.create_name(CONTROL_NAME);
        let mapping = Mapping::create(&control_name, sa)
            .with_context(|| format!("CreateFileMapping {control_name}"))?;
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
            .context("ID3D11Device5::CreateFence(SHARED) (needs Windows 10 1703+)")?;
        let fence = fence.context("CreateFence returned no fence")?;
        let fence_name = scope.create_name(&fence_base(pid));
        // SAFETY: `sa` outlives the call; the name is a valid wide string.
        let handle = unsafe {
            fence.CreateSharedHandle(
                Some(sa.as_ptr()),
                GENERIC_ALL.0,
                &HSTRING::from(&fence_name),
            )
        }
        .with_context(|| format!("ID3D11Fence::CreateSharedHandle {fence_name}"))?;
        handles.push(OwnedHandle(handle));
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
) -> Result<(ID3D11Texture2D, OwnedHandle)> {
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
        .context("CreateTexture2D (shared NT handle)")?;
    let tex: ID3D11Texture2D = tex.context("CreateTexture2D returned no texture")?;
    let res: IDXGIResource1 = tex.cast().context("IDXGIResource1")?;
    // SAFETY: `sa` outlives the call; the name is a valid wide string.
    let handle = unsafe {
        res.CreateSharedHandle(
            Some(sa.as_ptr()),
            DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
            &HSTRING::from(name),
        )
    }
    .with_context(|| format!("IDXGIResource1::CreateSharedHandle {name}"))?;
    Ok((tex, OwnedHandle(handle)))
}

/// An immutable texture filled with one BGRA colour.
fn solid(device: &ID3D11Device5, w: u32, h: u32, bgra: [u8; 4]) -> Result<ID3D11Texture2D> {
    let pixels: Vec<u8> = bgra
        .iter()
        .copied()
        .cycle()
        .take((w * h * 4) as usize)
        .collect();
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
        .context("CreateTexture2D (solid)")?;
    tex.context("CreateTexture2D returned no texture")
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
    .context("D3D11CreateDevice (hardware)")?;
    let device = device.context("no device")?;
    let context = context.context("no context")?;
    Ok((
        device
            .cast()
            .context("ID3D11Device5 (needs Windows 10 1703+ for shared fences)")?,
        context.cast().context("ID3D11DeviceContext4")?,
    ))
}

fn adapter_facts(device: &ID3D11Device5) -> Result<(String, u64)> {
    let dxgi: IDXGIDevice = device.cast().context("IDXGIDevice")?;
    // SAFETY: plain calls on live COM objects.
    let desc = unsafe { dxgi.GetAdapter().and_then(|a| a.GetDesc()) }.context("adapter desc")?;
    let len = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    let luid = ((desc.AdapterLuid.HighPart as u32 as u64) << 32) | desc.AdapterLuid.LowPart as u64;
    Ok((String::from_utf16_lossy(&desc.Description[..len]), luid))
}
