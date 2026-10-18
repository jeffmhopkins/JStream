//! libva's functions, loaded at run time (ADR-0060 §8), and a VA display on one DRM
//! render node.
//!
//! Each function type below is its C declaration in libva 2.20.0's `va/va.h` or
//! `va/va_drm.h`, with `VADisplay` as `void *`, the enums `VAStatus`, `VAProfile`,
//! `VAEntrypoint`, `VAConfigAttribType`, `VABufferType` and `VASurfaceStatus` as `int`,
//! and the ID types (`VAConfigID`, `VASurfaceID`, ...) as `unsigned int`
//! ([`Id`]). The structs they take are in `ffi`.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::sync::Arc;

use jstream_platform::PlatformError;

use super::ffi::{ConfigAttrib, Id, Image, ImageFormat, STATUS_SUCCESS, Status};
use crate::dl::SystemLibrary;

pub(crate) type VaDisplay = *mut c_void;

/// `VADisplay vaGetDisplayDRM(int fd)`.
type GetDisplayDrm = unsafe extern "C" fn(fd: c_int) -> VaDisplay;
/// `VAStatus vaInitialize(VADisplay dpy, int *major_version, int *minor_version)`.
type Initialize = unsafe extern "C" fn(VaDisplay, *mut c_int, *mut c_int) -> Status;
/// `VAStatus vaTerminate(VADisplay dpy)`.
type Terminate = unsafe extern "C" fn(VaDisplay) -> Status;
/// `const char *vaQueryVendorString(VADisplay dpy)`.
type QueryVendorString = unsafe extern "C" fn(VaDisplay) -> *const c_char;
/// `const char *vaErrorStr(VAStatus error_status)`.
type ErrorStr = unsafe extern "C" fn(Status) -> *const c_char;
/// `int vaMaxNumEntrypoints(VADisplay dpy)`.
type MaxNumEntrypoints = unsafe extern "C" fn(VaDisplay) -> c_int;
/// `VAStatus vaQueryConfigEntrypoints(VADisplay dpy, VAProfile profile,
/// VAEntrypoint *entrypoint_list, int *num_entrypoints)`.
type QueryConfigEntrypoints =
    unsafe extern "C" fn(VaDisplay, c_int, *mut c_int, *mut c_int) -> Status;
/// `VAStatus vaGetConfigAttributes(VADisplay dpy, VAProfile profile,
/// VAEntrypoint entrypoint, VAConfigAttrib *attrib_list, int num_attribs)`.
type GetConfigAttributes =
    unsafe extern "C" fn(VaDisplay, c_int, c_int, *mut ConfigAttrib, c_int) -> Status;
/// `VAStatus vaCreateConfig(VADisplay dpy, VAProfile profile, VAEntrypoint entrypoint,
/// VAConfigAttrib *attrib_list, int num_attribs, VAConfigID *config_id)`.
type CreateConfig =
    unsafe extern "C" fn(VaDisplay, c_int, c_int, *mut ConfigAttrib, c_int, *mut Id) -> Status;
/// `VAStatus vaDestroyConfig(VADisplay dpy, VAConfigID config_id)`.
type DestroyConfig = unsafe extern "C" fn(VaDisplay, Id) -> Status;
/// `VAStatus vaCreateSurfaces(VADisplay dpy, unsigned int format, unsigned int width,
/// unsigned int height, VASurfaceID *surfaces, unsigned int num_surfaces,
/// VASurfaceAttrib *attrib_list, unsigned int num_attribs)`.
type CreateSurfaces = unsafe extern "C" fn(
    VaDisplay,
    c_uint,
    c_uint,
    c_uint,
    *mut Id,
    c_uint,
    *mut c_void,
    c_uint,
) -> Status;
/// `VAStatus vaDestroySurfaces(VADisplay dpy, VASurfaceID *surfaces, int num_surfaces)`.
type DestroySurfaces = unsafe extern "C" fn(VaDisplay, *mut Id, c_int) -> Status;
/// `VAStatus vaCreateContext(VADisplay dpy, VAConfigID config_id, int picture_width,
/// int picture_height, int flag, VASurfaceID *render_targets, int num_render_targets,
/// VAContextID *context)`.
type CreateContext =
    unsafe extern "C" fn(VaDisplay, Id, c_int, c_int, c_int, *mut Id, c_int, *mut Id) -> Status;
/// `VAStatus vaDestroyContext(VADisplay dpy, VAContextID context)`.
type DestroyContext = unsafe extern "C" fn(VaDisplay, Id) -> Status;
/// `VAStatus vaCreateBuffer(VADisplay dpy, VAContextID context, VABufferType type,
/// unsigned int size, unsigned int num_elements, void *data, VABufferID *buf_id)`.
type CreateBuffer =
    unsafe extern "C" fn(VaDisplay, Id, c_int, c_uint, c_uint, *mut c_void, *mut Id) -> Status;
/// `VAStatus vaDestroyBuffer(VADisplay dpy, VABufferID buffer_id)`.
type DestroyBuffer = unsafe extern "C" fn(VaDisplay, Id) -> Status;
/// `VAStatus vaMapBuffer(VADisplay dpy, VABufferID buf_id, void **pbuf)`.
type MapBuffer = unsafe extern "C" fn(VaDisplay, Id, *mut *mut c_void) -> Status;
/// `VAStatus vaUnmapBuffer(VADisplay dpy, VABufferID buf_id)`.
type UnmapBuffer = unsafe extern "C" fn(VaDisplay, Id) -> Status;
/// `VAStatus vaBeginPicture(VADisplay dpy, VAContextID context, VASurfaceID
/// render_target)`.
type BeginPicture = unsafe extern "C" fn(VaDisplay, Id, Id) -> Status;
/// `VAStatus vaRenderPicture(VADisplay dpy, VAContextID context, VABufferID *buffers,
/// int num_buffers)`.
type RenderPicture = unsafe extern "C" fn(VaDisplay, Id, *mut Id, c_int) -> Status;
/// `VAStatus vaEndPicture(VADisplay dpy, VAContextID context)`.
type EndPicture = unsafe extern "C" fn(VaDisplay, Id) -> Status;
/// `VAStatus vaSyncSurface(VADisplay dpy, VASurfaceID render_target)`.
type SyncSurface = unsafe extern "C" fn(VaDisplay, Id) -> Status;
/// `VAStatus vaQuerySurfaceStatus(VADisplay dpy, VASurfaceID render_target,
/// VASurfaceStatus *status)`.
type QuerySurfaceStatus = unsafe extern "C" fn(VaDisplay, Id, *mut c_int) -> Status;
/// `VAStatus vaDeriveImage(VADisplay dpy, VASurfaceID surface, VAImage *image)`.
type DeriveImage = unsafe extern "C" fn(VaDisplay, Id, *mut Image) -> Status;
/// `VAStatus vaCreateImage(VADisplay dpy, VAImageFormat *format, int width, int height,
/// VAImage *image)`.
type CreateImage =
    unsafe extern "C" fn(VaDisplay, *mut ImageFormat, c_int, c_int, *mut Image) -> Status;
/// `VAStatus vaDestroyImage(VADisplay dpy, VAImageID image)`.
type DestroyImage = unsafe extern "C" fn(VaDisplay, Id) -> Status;
/// `VAStatus vaPutImage(VADisplay dpy, VASurfaceID surface, VAImageID image, int src_x,
/// int src_y, unsigned int src_width, unsigned int src_height, int dest_x, int dest_y,
/// unsigned int dest_width, unsigned int dest_height)`.
type PutImage = unsafe extern "C" fn(
    VaDisplay,
    Id,
    Id,
    c_int,
    c_int,
    c_uint,
    c_uint,
    c_int,
    c_int,
    c_uint,
    c_uint,
) -> Status;

/// The libva functions the crate calls, with the libraries that hold them.
pub(crate) struct Va {
    pub(crate) get_display_drm: GetDisplayDrm,
    pub(crate) initialize: Initialize,
    pub(crate) terminate: Terminate,
    pub(crate) query_vendor_string: QueryVendorString,
    pub(crate) error_str: ErrorStr,
    pub(crate) max_num_entrypoints: MaxNumEntrypoints,
    pub(crate) query_config_entrypoints: QueryConfigEntrypoints,
    pub(crate) get_config_attributes: GetConfigAttributes,
    pub(crate) create_config: CreateConfig,
    pub(crate) destroy_config: DestroyConfig,
    pub(crate) create_surfaces: CreateSurfaces,
    pub(crate) destroy_surfaces: DestroySurfaces,
    pub(crate) create_context: CreateContext,
    pub(crate) destroy_context: DestroyContext,
    pub(crate) create_buffer: CreateBuffer,
    pub(crate) destroy_buffer: DestroyBuffer,
    pub(crate) map_buffer: MapBuffer,
    pub(crate) unmap_buffer: UnmapBuffer,
    pub(crate) begin_picture: BeginPicture,
    pub(crate) render_picture: RenderPicture,
    pub(crate) end_picture: EndPicture,
    pub(crate) sync_surface: SyncSurface,
    pub(crate) query_surface_status: QuerySurfaceStatus,
    pub(crate) derive_image: DeriveImage,
    pub(crate) create_image: CreateImage,
    pub(crate) destroy_image: DestroyImage,
    pub(crate) put_image: PutImage,
    _va: SystemLibrary,
    _drm: SystemLibrary,
}

impl Va {
    /// Loads libva and libva-drm.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when either is missing or lacks a function.
    pub(crate) fn load() -> Result<Arc<Self>, PlatformError> {
        let remedy = "install libva (Debian and Ubuntu: libva2 and libva-drm2; Fedora: \
                      libva) and your GPU's VA-API driver";
        let va = SystemLibrary::open("libva", &["libva.so.2"], remedy)?;
        let drm = SystemLibrary::open("libva-drm", &["libva-drm.so.2"], remedy)?;
        // SAFETY: each field's type is the C declaration of the symbol it is resolved
        // from (the aliases above, each with its prototype), and both libraries stay
        // loaded in `Va` for as long as the pointers can be called.
        unsafe {
            Ok(Arc::new(Self {
                get_display_drm: drm.function("vaGetDisplayDRM")?,
                initialize: va.function("vaInitialize")?,
                terminate: va.function("vaTerminate")?,
                query_vendor_string: va.function("vaQueryVendorString")?,
                error_str: va.function("vaErrorStr")?,
                max_num_entrypoints: va.function("vaMaxNumEntrypoints")?,
                query_config_entrypoints: va.function("vaQueryConfigEntrypoints")?,
                get_config_attributes: va.function("vaGetConfigAttributes")?,
                create_config: va.function("vaCreateConfig")?,
                destroy_config: va.function("vaDestroyConfig")?,
                create_surfaces: va.function("vaCreateSurfaces")?,
                destroy_surfaces: va.function("vaDestroySurfaces")?,
                create_context: va.function("vaCreateContext")?,
                destroy_context: va.function("vaDestroyContext")?,
                create_buffer: va.function("vaCreateBuffer")?,
                destroy_buffer: va.function("vaDestroyBuffer")?,
                map_buffer: va.function("vaMapBuffer")?,
                unmap_buffer: va.function("vaUnmapBuffer")?,
                begin_picture: va.function("vaBeginPicture")?,
                render_picture: va.function("vaRenderPicture")?,
                end_picture: va.function("vaEndPicture")?,
                sync_surface: va.function("vaSyncSurface")?,
                query_surface_status: va.function("vaQuerySurfaceStatus")?,
                derive_image: va.function("vaDeriveImage")?,
                create_image: va.function("vaCreateImage")?,
                destroy_image: va.function("vaDestroyImage")?,
                put_image: va.function("vaPutImage")?,
                _va: va,
                _drm: drm,
            }))
        }
    }

    /// `Ok` for `VA_STATUS_SUCCESS`, otherwise [`PlatformError::Os`] naming `call`, with
    /// the status and libva's description of it.
    pub(crate) fn check(&self, call: &'static str, status: Status) -> Result<(), PlatformError> {
        if status == STATUS_SUCCESS {
            return Ok(());
        }
        // SAFETY: vaErrorStr takes any status and returns a static NUL-terminated
        // string (or null, handled).
        let ptr = unsafe { (self.error_str)(status) };
        let message = if ptr.is_null() {
            "unknown VA-API error".to_owned()
        } else {
            // SAFETY: non-null, NUL-terminated and static.
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        };
        Err(PlatformError::Os {
            call,
            code: Some(status as u32),
            message,
        })
    }
}

/// An initialised VA display on one render node. Terminated on drop, before the node is
/// closed.
pub(crate) struct Display {
    pub(crate) va: Arc<Va>,
    pub(crate) dpy: VaDisplay,
    // Dropped after `drop` below has terminated the display.
    _node: File,
}

impl Display {
    /// Opens `node` and initialises a display on it.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the node can't be opened or no driver initialises on
    /// it.
    pub(crate) fn open(va: Arc<Va>, node: &str) -> Result<Self, PlatformError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .map_err(|e| PlatformError::Os {
                call: "open (DRM render node)",
                code: e.raw_os_error().map(|c| c as u32),
                message: format!("{node}: {e}"),
            })?;
        // SAFETY: the fd is open for the display's lifetime (`_node`); libva only
        // stores it.
        // C: `VADisplay vaGetDisplayDRM(int fd)`.
        let dpy = unsafe { (va.get_display_drm)(file.as_raw_fd()) };
        if dpy.is_null() {
            return Err(PlatformError::Os {
                call: "vaGetDisplayDRM",
                code: None,
                message: format!("no VA display on {node}"),
            });
        }
        let (mut major, mut minor) = (0, 0);
        // SAFETY: `dpy` came from vaGetDisplayDRM; the out-pointers are valid.
        // C: `VAStatus vaInitialize(VADisplay, int *major, int *minor)`.
        let status = unsafe { (va.initialize)(dpy, &mut major, &mut minor) };
        if let Err(e) = va.check("vaInitialize", status) {
            // SAFETY: vaTerminate releases a display even when initialisation failed.
            // C: `VAStatus vaTerminate(VADisplay)`.
            unsafe { (va.terminate)(dpy) };
            return Err(e);
        }
        Ok(Self {
            va,
            dpy,
            _node: file,
        })
    }

    pub(crate) fn vendor(&self) -> String {
        // SAFETY: `dpy` is initialised; the result is a NUL-terminated string owned by
        // the driver for the display's lifetime, or null.
        // C: `const char *vaQueryVendorString(VADisplay)`.
        let ptr = unsafe { (self.va.query_vendor_string)(self.dpy) };
        if ptr.is_null() {
            return String::new();
        }
        // SAFETY: non-null and NUL-terminated, as `vaQueryVendorString` returns it (see
        // above), and copied before the display drops.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    /// The raw entry points the driver offers for `profile` (a `VAProfile`).
    pub(crate) fn entrypoints(&self, profile: c_int) -> Vec<i32> {
        // SAFETY: `dpy` is initialised.
        // C: `int vaMaxNumEntrypoints(VADisplay)`.
        let max = unsafe { (self.va.max_num_entrypoints)(self.dpy) };
        let Ok(cap) = usize::try_from(max) else {
            return Vec::new();
        };
        let mut list = vec![0 as c_int; cap];
        let mut count: c_int = 0;
        // SAFETY: `list` holds `vaMaxNumEntrypoints` entries, the most libva writes;
        // `count` is a valid out-pointer.
        // C: `VAStatus vaQueryConfigEntrypoints(VADisplay, VAProfile,
        // VAEntrypoint *entrypoint_list, int *num_entrypoints)`.
        let status = unsafe {
            (self.va.query_config_entrypoints)(self.dpy, profile, list.as_mut_ptr(), &mut count)
        };
        if status != STATUS_SUCCESS {
            // VA_STATUS_ERROR_UNSUPPORTED_PROFILE and the like: none for this profile.
            return Vec::new();
        }
        list.truncate(usize::try_from(count).unwrap_or(0).min(cap));
        list
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: `dpy` is initialised and terminated exactly once, here.
        // C: `VAStatus vaTerminate(VADisplay)`.
        unsafe { (self.va.terminate)(self.dpy) };
    }
}
