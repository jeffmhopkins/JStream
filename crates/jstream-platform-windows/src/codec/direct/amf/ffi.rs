//! The parts of AMD's Advanced Media Framework (AMF) runtime API the AMF encoder calls,
//! declared by hand from the C (not C++) definitions in AMD's public AMF headers
//! (`amf/public/include`, MIT licence, Copyright (c) Advanced Micro Devices, Inc.:
//! <https://github.com/GPUOpen-LibrariesAndSDKs/AMF>). The runtime, `amfrt64.dll`, is
//! installed by AMD's display driver and loaded at run time, so nothing here links.
//!
//! Only what the encoder calls is declared. Every vtable is complete up to its last
//! slot, with the slots it doesn't call kept as opaque pointers, so each struct has the
//! size and the slot offsets of the C one. Those were read from clang's MSVC x64 layout
//! of the headers (`clang -cc1 -triple x86_64-pc-windows-msvc`, compiling `core/Factory.h`
//! and the three encoder headers as C, with `offsetof`/`sizeof` emitted as globals), and
//! are asserted at compile time at the end of this file, on every platform. The C
//! headers already order `AMFPropertyStorageEx`'s overloaded methods as MSVC lays out the
//! runtime's C++ vtable (reversed on Windows).
//!
//! The property names, IDs and enum values are the headers' too; the enum values were
//! read from the same compilation.

// The declarations below are transcribed from AMD's AMF headers, whose notice follows,
// kept as their licence asks:
//
// MIT license
//
// Copyright (c) 2018 Advanced Micro Devices, Inc. All rights reserved.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of this
// software and associated documentation files (the "Software"), to deal in the Software
// without restriction, including without limitation the rights to use, copy, modify,
// merge, publish, distribute, sublicense, and/or sell copies of the Software, and to
// permit persons to whom the Software is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice shall be included in all copies
// or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
// INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
// HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
// CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE
// OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

use core::ffi::c_void;

/// `AMF_RESULT`: a C enum, kept as its integer so an unknown value is never undefined.
pub(crate) type AmfResult = i32;

/// `AMF_OK`.
pub(crate) const AMF_OK: AmfResult = 0;
/// `AMF_NOT_FOUND`: e.g. a property the runtime doesn't know.
pub(crate) const AMF_NOT_FOUND: AmfResult = 11;
/// `AMF_EOF`: `QueryOutput` after a drain has returned everything.
pub(crate) const AMF_EOF: AmfResult = 23;
/// `AMF_REPEAT`: `QueryOutput` has nothing yet.
pub(crate) const AMF_REPEAT: AmfResult = 24;
/// `AMF_INPUT_FULL`: `SubmitInput` (or `Drain`) can't take more until output is taken.
pub(crate) const AMF_INPUT_FULL: AmfResult = 25;
/// `AMF_NEED_MORE_INPUT`: no output until more input; not an error.
pub(crate) const AMF_NEED_MORE_INPUT: AmfResult = 44;

/// The names of `AMF_RESULT`'s values from 0 to 45, in order.
const RESULT_NAMES: [&str; 46] = [
    "AMF_OK",
    "AMF_FAIL",
    "AMF_UNEXPECTED",
    "AMF_ACCESS_DENIED",
    "AMF_INVALID_ARG",
    "AMF_OUT_OF_RANGE",
    "AMF_OUT_OF_MEMORY",
    "AMF_INVALID_POINTER",
    "AMF_NO_INTERFACE",
    "AMF_NOT_IMPLEMENTED",
    "AMF_NOT_SUPPORTED",
    "AMF_NOT_FOUND",
    "AMF_ALREADY_INITIALIZED",
    "AMF_NOT_INITIALIZED",
    "AMF_INVALID_FORMAT",
    "AMF_WRONG_STATE",
    "AMF_FILE_NOT_OPEN",
    "AMF_NO_DEVICE",
    "AMF_DIRECTX_FAILED",
    "AMF_OPENCL_FAILED",
    "AMF_GLX_FAILED",
    "AMF_XV_FAILED",
    "AMF_ALSA_FAILED",
    "AMF_EOF",
    "AMF_REPEAT",
    "AMF_INPUT_FULL",
    "AMF_RESOLUTION_CHANGED",
    "AMF_RESOLUTION_UPDATED",
    "AMF_INVALID_DATA_TYPE",
    "AMF_INVALID_RESOLUTION",
    "AMF_CODEC_NOT_SUPPORTED",
    "AMF_SURFACE_FORMAT_NOT_SUPPORTED",
    "AMF_SURFACE_MUST_BE_SHARED",
    "AMF_DECODER_NOT_PRESENT",
    "AMF_DECODER_SURFACE_ALLOCATION_FAILED",
    "AMF_DECODER_NO_FREE_SURFACES",
    "AMF_ENCODER_NOT_PRESENT",
    "AMF_DEM_ERROR",
    "AMF_DEM_PROPERTY_READONLY",
    "AMF_DEM_REMOTE_DISPLAY_CREATE_FAILED",
    "AMF_DEM_START_ENCODING_FAILED",
    "AMF_DEM_QUERY_OUTPUT_FAILED",
    "AMF_TAN_CLIPPING_WAS_REQUIRED",
    "AMF_TAN_UNSUPPORTED_VERSION",
    "AMF_NEED_MORE_INPUT",
    "AMF_VULKAN_FAILED",
];

/// The name of an `AMF_RESULT`, for messages.
pub(crate) fn result_name(result: AmfResult) -> &'static str {
    usize::try_from(result)
        .ok()
        .and_then(|i| RESULT_NAMES.get(i).copied())
        .unwrap_or("an unknown AMF_RESULT")
}

/// `AMF_MAKE_FULL_VERSION`.
pub(crate) const fn full_version(major: u64, minor: u64, release: u64, build: u64) -> u64 {
    (major << 48) | (minor << 32) | (release << 16) | build
}

/// A full version as `major.minor.release`, for messages.
pub(crate) fn version_name(version: u64) -> String {
    format!(
        "{}.{}.{}",
        version >> 48,
        (version >> 32) & 0xFFFF,
        (version >> 16) & 0xFFFF
    )
}

/// The runtime DLL, `AMF_DLL_NAMEA` on 64-bit Windows.
pub(crate) const DLL: &str = "amfrt64.dll";
/// `AMF_INIT_FUNCTION_NAME`, null-terminated.
pub(crate) const INIT_FUNCTION: &[u8] = b"AMFInit\0";
/// `AMF_QUERY_VERSION_FUNCTION_NAME`, null-terminated.
pub(crate) const QUERY_VERSION_FUNCTION: &[u8] = b"AMFQueryVersion\0";

/// `AMFInit_Fn` (`AMF_CDECL_CALL`, which is the one x64 convention).
pub(crate) type AmfInitFn =
    unsafe extern "C" fn(version: u64, factory: *mut *mut AMFFactory) -> AmfResult;
/// `AMFQueryVersion_Fn`.
pub(crate) type AmfQueryVersionFn = unsafe extern "C" fn(version: *mut u64) -> AmfResult;

/// `AMF_SURFACE_NV12`.
pub(crate) const AMF_SURFACE_NV12: i32 = 1;
/// `AMF_DX11_0`, the D3D11 feature level `InitDX11` asks for.
pub(crate) const AMF_DX11_0: i32 = 110;

/// `AMF_VARIANT_BOOL`.
pub(crate) const AMF_VARIANT_BOOL: i32 = 1;
/// `AMF_VARIANT_INT64`.
pub(crate) const AMF_VARIANT_INT64: i32 = 2;
/// `AMF_VARIANT_SIZE`.
pub(crate) const AMF_VARIANT_SIZE: i32 = 5;
/// `AMF_VARIANT_RATE`.
pub(crate) const AMF_VARIANT_RATE: i32 = 7;

/// `AMFGuid`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AMFGuid {
    pub(crate) data1: u32,
    pub(crate) data2: u16,
    pub(crate) data3: u16,
    pub(crate) data4: [u8; 8],
}

/// `IID_AMFBuffer`, to ask an output `AMFData` for its `AMFBuffer`.
pub(crate) const IID_AMF_BUFFER: AMFGuid = AMFGuid {
    data1: 0xb04b_7248,
    data2: 0xb6f0,
    data3: 0x4321,
    data4: [0xb6, 0x91, 0xba, 0xa4, 0x74, 0x0f, 0x9f, 0xcb],
};

/// `AMFSize`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AMFSize {
    pub(crate) width: i32,
    pub(crate) height: i32,
}

/// `AMFRate`: frames per second as a fraction.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AMFRate {
    pub(crate) num: u32,
    pub(crate) den: u32,
}

/// The value union of `AMFVariantStruct`, with the members the encoder uses; `raw`
/// gives it the C union's 16 bytes (`AMFRect`, `AMFFloatVector4D`).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union AMFVariantValue {
    pub(crate) bool_value: u8,
    pub(crate) int64_value: i64,
    pub(crate) size_value: AMFSize,
    pub(crate) rate_value: AMFRate,
    pub(crate) raw: [u64; 2],
}

/// `AMFVariantStruct`: a property's type and value.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct AMFVariantStruct {
    /// `AMF_VARIANT_TYPE`.
    pub(crate) kind: i32,
    pub(crate) value: AMFVariantValue,
}

impl AMFVariantStruct {
    /// `AMF_VARIANT_EMPTY`, for `GetProperty` to fill.
    pub(crate) fn empty() -> Self {
        Self {
            kind: 0,
            value: AMFVariantValue { raw: [0; 2] },
        }
    }

    /// The value if it is an `amf_int64` (or an `amf_bool`, as 0 or 1).
    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self.kind {
            // SAFETY: `kind` says the int64 member is the one set.
            AMF_VARIANT_INT64 => Some(unsafe { self.value.int64_value }),
            // SAFETY: `kind` says the bool member is the one set.
            AMF_VARIANT_BOOL => Some(i64::from(unsafe { self.value.bool_value } != 0)),
            _ => None,
        }
    }
}

impl core::fmt::Debug for AMFVariantStruct {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AMFVariantStruct")
            .field("kind", &self.kind)
            .field("int64", &self.as_i64())
            .finish()
    }
}

/// A vtable slot the encoder never calls.
type Slot = *const c_void;

/// `AMFFactory`.
#[repr(C)]
pub(crate) struct AMFFactory {
    pub(crate) vtbl: *const AMFFactoryVtbl,
}

/// `AMFFactoryVtbl`.
#[repr(C)]
pub(crate) struct AMFFactoryVtbl {
    pub(crate) create_context: unsafe extern "system" fn(
        this: *mut AMFFactory,
        context: *mut *mut AMFContext,
    ) -> AmfResult,
    pub(crate) create_component: unsafe extern "system" fn(
        this: *mut AMFFactory,
        context: *mut AMFContext,
        id: *const u16,
        component: *mut *mut AMFComponent,
    ) -> AmfResult,
    /// SetCacheFolder, GetCacheFolder, GetDebug, GetTrace, GetPrograms.
    _rest: [Slot; 5],
}

/// `AMFContext`.
#[repr(C)]
pub(crate) struct AMFContext {
    pub(crate) vtbl: *const AMFContextVtbl,
}

/// `AMFContextVtbl`.
#[repr(C)]
pub(crate) struct AMFContextVtbl {
    _acquire: Slot,
    pub(crate) release: unsafe extern "system" fn(this: *mut AMFContext) -> i32,
    _query_interface: Slot,
    /// `AMFPropertyStorage`'s ten methods.
    _storage: [Slot; 10],
    pub(crate) terminate: unsafe extern "system" fn(this: *mut AMFContext) -> AmfResult,
    /// InitDX9, GetDX9Device, LockDX9, UnlockDX9.
    _dx9: [Slot; 4],
    pub(crate) init_dx11: unsafe extern "system" fn(
        this: *mut AMFContext,
        device: *mut c_void,
        version: i32,
    ) -> AmfResult,
    /// GetDX11Device through CreateSurfaceFromDX9Native: DX11's other three, OpenCL's
    /// eight, OpenGL's five, XV's four, Gralloc's four, the three allocations and the
    /// host and DX9 wraps.
    _middle: [Slot; 30],
    pub(crate) create_surface_from_dx11_native: unsafe extern "system" fn(
        this: *mut AMFContext,
        texture: *mut c_void,
        surface: *mut *mut AMFSurface,
        observer: *mut c_void,
    ) -> AmfResult,
    /// The OpenGL, Gralloc and OpenCL wraps and GetCompute.
    _rest: [Slot; 5],
}

/// `AMFComponent`.
#[repr(C)]
pub(crate) struct AMFComponent {
    pub(crate) vtbl: *const AMFComponentVtbl,
}

/// `AMFComponentVtbl`: `AMFInterface`, `AMFPropertyStorage`, `AMFPropertyStorageEx`,
/// then `AMFComponent`'s own methods.
#[repr(C)]
pub(crate) struct AMFComponentVtbl {
    _acquire: Slot,
    pub(crate) release: unsafe extern "system" fn(this: *mut AMFComponent) -> i32,
    _query_interface: Slot,
    pub(crate) set_property: unsafe extern "system" fn(
        this: *mut AMFComponent,
        name: *const u16,
        value: AMFVariantStruct,
    ) -> AmfResult,
    pub(crate) get_property: unsafe extern "system" fn(
        this: *mut AMFComponent,
        name: *const u16,
        value: *mut AMFVariantStruct,
    ) -> AmfResult,
    /// HasProperty through RemoveObserver.
    _storage: [Slot; 8],
    /// GetPropertiesInfoCount, GetPropertyInfo, GetPropertyInfoAt, ValidateProperty.
    _storage_ex: [Slot; 4],
    pub(crate) init: unsafe extern "system" fn(
        this: *mut AMFComponent,
        format: i32,
        width: i32,
        height: i32,
    ) -> AmfResult,
    _reinit: Slot,
    pub(crate) terminate: unsafe extern "system" fn(this: *mut AMFComponent) -> AmfResult,
    pub(crate) drain: unsafe extern "system" fn(this: *mut AMFComponent) -> AmfResult,
    _flush: Slot,
    pub(crate) submit_input:
        unsafe extern "system" fn(this: *mut AMFComponent, data: *mut AMFData) -> AmfResult,
    pub(crate) query_output:
        unsafe extern "system" fn(this: *mut AMFComponent, data: *mut *mut AMFData) -> AmfResult,
    /// GetContext, SetOutputDataAllocatorCB, GetCaps, Optimize.
    _rest: [Slot; 4],
}

/// `AMFData`, the base of `AMFSurface` and `AMFBuffer` (single inheritance, so a pointer
/// to either is a pointer to its `AMFData`).
#[repr(C)]
pub(crate) struct AMFData {
    pub(crate) vtbl: *const AMFDataVtbl,
}

/// `AMFDataVtbl`: `AMFInterface`, `AMFPropertyStorage`, then `AMFData`'s own methods.
#[repr(C)]
pub(crate) struct AMFDataVtbl {
    _acquire: Slot,
    pub(crate) release: unsafe extern "system" fn(this: *mut AMFData) -> i32,
    pub(crate) query_interface: unsafe extern "system" fn(
        this: *mut AMFData,
        iid: *const AMFGuid,
        out: *mut *mut c_void,
    ) -> AmfResult,
    pub(crate) set_property: unsafe extern "system" fn(
        this: *mut AMFData,
        name: *const u16,
        value: AMFVariantStruct,
    ) -> AmfResult,
    pub(crate) get_property: unsafe extern "system" fn(
        this: *mut AMFData,
        name: *const u16,
        value: *mut AMFVariantStruct,
    ) -> AmfResult,
    /// HasProperty through RemoveObserver.
    _storage: [Slot; 8],
    /// GetMemoryType, Duplicate, Convert, Interop, GetDataType, IsReusable.
    _data: [Slot; 6],
    pub(crate) set_pts: unsafe extern "system" fn(this: *mut AMFData, pts: i64),
    pub(crate) get_pts: unsafe extern "system" fn(this: *mut AMFData) -> i64,
    pub(crate) set_duration: unsafe extern "system" fn(this: *mut AMFData, duration: i64),
    _get_duration: Slot,
}

/// `AMFSurface`. Called only through its `AMFData` part.
#[repr(C)]
pub(crate) struct AMFSurface {
    pub(crate) vtbl: *const AMFSurfaceVtbl,
}

/// `AMFSurfaceVtbl`: `AMFData`'s, then the surface's own ten.
#[repr(C)]
pub(crate) struct AMFSurfaceVtbl {
    pub(crate) data: AMFDataVtbl,
    _surface: [Slot; 10],
}

/// `AMFBuffer`.
#[repr(C)]
pub(crate) struct AMFBuffer {
    pub(crate) vtbl: *const AMFBufferVtbl,
}

/// `AMFBufferVtbl`: `AMFData`'s, then the buffer's own five.
#[repr(C)]
pub(crate) struct AMFBufferVtbl {
    pub(crate) data: AMFDataVtbl,
    _set_size: Slot,
    pub(crate) get_size: unsafe extern "system" fn(this: *mut AMFBuffer) -> usize,
    pub(crate) get_native: unsafe extern "system" fn(this: *mut AMFBuffer) -> *mut c_void,
    /// AddObserver, RemoveObserver.
    _observers: [Slot; 2],
}

// The layouts clang gives the C headers for x86_64-pc-windows-msvc (module docs).
const _: () = {
    use core::mem::{align_of, offset_of, size_of};
    assert!(size_of::<AMFVariantStruct>() == 24);
    assert!(align_of::<AMFVariantStruct>() == 8);
    assert!(offset_of!(AMFVariantStruct, value) == 8);
    assert!(size_of::<AMFGuid>() == 16);
    assert!(size_of::<AMFRate>() == 8);
    assert!(size_of::<AMFSize>() == 8);
    assert!(size_of::<AMFFactory>() == 8);
    assert!(size_of::<AMFContext>() == 8);
    assert!(size_of::<AMFComponent>() == 8);
    assert!(size_of::<AMFData>() == 8);
    assert!(size_of::<AMFSurface>() == 8);
    assert!(size_of::<AMFBuffer>() == 8);

    assert!(size_of::<AMFFactoryVtbl>() == 56);
    assert!(offset_of!(AMFFactoryVtbl, create_context) == 0);
    assert!(offset_of!(AMFFactoryVtbl, create_component) == 8);

    assert!(size_of::<AMFContextVtbl>() == 440);
    assert!(offset_of!(AMFContextVtbl, release) == 8);
    assert!(offset_of!(AMFContextVtbl, terminate) == 104);
    assert!(offset_of!(AMFContextVtbl, init_dx11) == 144);
    assert!(offset_of!(AMFContextVtbl, create_surface_from_dx11_native) == 392);

    assert!(size_of::<AMFComponentVtbl>() == 224);
    assert!(offset_of!(AMFComponentVtbl, release) == 8);
    assert!(offset_of!(AMFComponentVtbl, set_property) == 24);
    assert!(offset_of!(AMFComponentVtbl, get_property) == 32);
    assert!(offset_of!(AMFComponentVtbl, _storage_ex) == 104);
    assert!(offset_of!(AMFComponentVtbl, init) == 136);
    assert!(offset_of!(AMFComponentVtbl, terminate) == 152);
    assert!(offset_of!(AMFComponentVtbl, drain) == 160);
    assert!(offset_of!(AMFComponentVtbl, submit_input) == 176);
    assert!(offset_of!(AMFComponentVtbl, query_output) == 184);

    assert!(size_of::<AMFDataVtbl>() == 184);
    assert!(offset_of!(AMFDataVtbl, release) == 8);
    assert!(offset_of!(AMFDataVtbl, query_interface) == 16);
    assert!(offset_of!(AMFDataVtbl, set_property) == 24);
    assert!(offset_of!(AMFDataVtbl, get_property) == 32);
    assert!(offset_of!(AMFDataVtbl, set_pts) == 152);
    assert!(offset_of!(AMFDataVtbl, get_pts) == 160);
    assert!(offset_of!(AMFDataVtbl, set_duration) == 168);

    assert!(size_of::<AMFSurfaceVtbl>() == 264);
    assert!(size_of::<AMFBufferVtbl>() == 224);
    assert!(offset_of!(AMFBufferVtbl, get_size) == 192);
    assert!(offset_of!(AMFBufferVtbl, get_native) == 200);
};

#[cfg(test)]
mod tests {
    use super::{
        AMF_EOF, AMF_INPUT_FULL, AMF_NEED_MORE_INPUT, AMF_OK, AMF_REPEAT, AMF_VARIANT_BOOL,
        AMF_VARIANT_INT64, AMFVariantStruct, AMFVariantValue, full_version, result_name,
        version_name,
    };

    #[test]
    fn results_are_named() {
        assert_eq!(result_name(AMF_OK), "AMF_OK");
        assert_eq!(result_name(AMF_EOF), "AMF_EOF");
        assert_eq!(result_name(AMF_REPEAT), "AMF_REPEAT");
        assert_eq!(result_name(AMF_INPUT_FULL), "AMF_INPUT_FULL");
        assert_eq!(result_name(AMF_NEED_MORE_INPUT), "AMF_NEED_MORE_INPUT");
        assert_eq!(result_name(36), "AMF_ENCODER_NOT_PRESENT");
        assert_eq!(result_name(-1), "an unknown AMF_RESULT");
        assert_eq!(result_name(999), "an unknown AMF_RESULT");
    }

    #[test]
    fn versions_pack_as_the_headers_do() {
        let v = full_version(1, 4, 29, 0);
        assert_eq!(v, 0x0001_0004_001D_0000);
        assert_eq!(version_name(v), "1.4.29");
        assert!(full_version(1, 5, 0, 0) > v);
    }

    #[test]
    fn variants_read_back_integers_and_bools() {
        let int = AMFVariantStruct {
            kind: AMF_VARIANT_INT64,
            value: AMFVariantValue { int64_value: -7 },
        };
        assert_eq!(int.as_i64(), Some(-7));
        let mut b = AMFVariantStruct::empty();
        b.kind = AMF_VARIANT_BOOL;
        b.value.bool_value = 1;
        assert_eq!(b.as_i64(), Some(1));
        assert_eq!(AMFVariantStruct::empty().as_i64(), None);
    }
}
