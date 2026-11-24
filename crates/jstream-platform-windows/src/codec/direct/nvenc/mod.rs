//! NVIDIA's NVENC, reached directly through the driver's `nvEncodeAPI64.dll`
//! (ADR-0217), on the context's D3D11 device.
//!
//! The session opens NVENC API 12.0 (driver 522 or later, which every GPU with an AV1
//! encoder has) on the context's D3D11 device, starts from the P5 preset tuned for high
//! quality (what OBS streams with) and sets what the engine needs on it ([`configure`]):
//! the bit rate (CBR, or VBR under a peak), a one-second VBV buffer, the keyframe
//! interval as the GOP and IDR period, no B-frames and no look-ahead, so packets come
//! out in input order with `dts == pts`, H.264 High, HEVC Main or AV1 Main at 8-bit
//! 4:2:0, BT.709 limited range in the VUI or sequence header, and the parameter sets
//! (or AV1 sequence header) repeated on every IDR frame. AV1 comes out as low-overhead
//! OBUs, not Annex B.
//!
//! Input is eight NV12 textures of the encoder's own (`Ring`), each registered with
//! NVENC once, and an output buffer and a completion event per texture: encoding is
//! asynchronous (`enableEncodeAsync`), so `encode` queues the copy and the picture and
//! returns, and `poll` takes the packet of every frame whose event has fired, oldest
//! first, without waiting. `finish` sends the end-of-stream picture and waits up to 5 s
//! for the rest.

#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "used by the Windows session; built elsewhere for its tests"
    )
)]
pub(crate) mod ffi;

use crate::codec::video::{Format, Settings};
use ffi::{Config, Guid};

#[cfg(windows)]
pub(crate) use session::NvencEncoder;

/// The NVENC codec and profile GUIDs of a format.
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "used by the Windows session; built elsewhere for its tests"
    )
)]
pub(crate) fn guids(format: Format) -> (Guid, Guid) {
    match format {
        Format::H264 => (ffi::CODEC_H264, ffi::H264_PROFILE_HIGH),
        Format::Hevc => (ffi::CODEC_HEVC, ffi::HEVC_PROFILE_MAIN),
        Format::Av1 => (ffi::CODEC_AV1, ffi::AV1_PROFILE_MAIN),
    }
}

/// Sets what the engine needs on `config`, the preset's configuration for
/// `settings.format`: profile, GOP, rate control, no B-frames or look-ahead, colour
/// description and in-band parameter sets on every IDR frame (module docs).
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "used by the Windows session; built elsewhere for its tests"
    )
)]
pub(crate) fn configure(config: &mut Config, settings: &Settings, keyframe_interval: u32) {
    config.version = ffi::CONFIG_VER;
    config.profile_guid = guids(settings.format).1;
    config.gop_length = keyframe_interval;
    // One P frame per period: no B-frames, so packets come out in input order.
    config.frame_interval_p = 1;
    config.mono_chrome_encoding = 0;
    config.frame_field_mode = ffi::FRAME_FIELD_MODE_FRAME;
    let rc = &mut config.rc_params;
    let peak = settings.max_bitrate.unwrap_or(settings.bitrate);
    rc.rate_control_mode = if settings.max_bitrate.is_some() {
        ffi::RC_VBR
    } else {
        ffi::RC_CBR
    };
    rc.average_bit_rate = settings.bitrate;
    rc.max_bit_rate = peak;
    // One second at the peak rate, as RTMP ingest expects of a CBR stream.
    rc.vbv_buffer_size = peak;
    rc.vbv_initial_delay = peak;
    ffi::set_bits(&mut rc.flags, ffi::RcParams::LOOKAHEAD, 1, 0);
    ffi::set_bits(&mut rc.flags, ffi::RcParams::ZERO_REORDER_DELAY, 1, 1);
    rc.lookahead_depth = 0;
    let bt709 = |vui: &mut ffi::VuiParameters| {
        vui.video_signal_type_present_flag = 1;
        vui.video_format = ffi::VUI_VIDEO_FORMAT_UNSPECIFIED;
        vui.video_full_range_flag = 0;
        vui.colour_description_present_flag = 1;
        vui.colour_primaries = ffi::VUI_BT709;
        vui.transfer_characteristics = ffi::VUI_BT709;
        vui.colour_matrix = ffi::VUI_BT709;
    };
    // SAFETY: each arm writes the union member of the codec the preset was read for;
    // every member is plain integers and pointers, valid for any bits.
    unsafe {
        match settings.format {
            Format::H264 => {
                let h264 = &mut config.codec.h264;
                h264.idr_period = keyframe_interval;
                ffi::set_bits(&mut h264.flags, ffi::ConfigH264::REPEAT_SPSPPS, 1, 1);
                ffi::set_bits(&mut h264.flags, ffi::ConfigH264::DISABLE_SPSPPS, 1, 0);
                ffi::set_bits(&mut h264.flags, ffi::ConfigH264::OUTPUT_AUD, 1, 0);
                h264.chroma_format_idc = 1;
                bt709(&mut h264.vui);
            }
            Format::Hevc => {
                let hevc = &mut config.codec.hevc;
                hevc.idr_period = keyframe_interval;
                ffi::set_bits(&mut hevc.flags, ffi::ConfigHevc::REPEAT_SPSPPS, 1, 1);
                ffi::set_bits(&mut hevc.flags, ffi::ConfigHevc::DISABLE_SPSPPS, 1, 0);
                ffi::set_bits(&mut hevc.flags, ffi::ConfigHevc::OUTPUT_AUD, 1, 0);
                ffi::set_bits(&mut hevc.flags, ffi::ConfigHevc::CHROMA_FORMAT_IDC, 2, 1);
                ffi::set_bits(
                    &mut hevc.flags,
                    ffi::ConfigHevc::PIXEL_BIT_DEPTH_MINUS8,
                    3,
                    0,
                );
                bt709(&mut hevc.vui);
            }
            Format::Av1 => {
                let av1 = &mut config.codec.av1;
                av1.idr_period = keyframe_interval;
                let flags = &mut av1.flags;
                ffi::set_bits(flags, ffi::ConfigAv1::OUTPUT_ANNEXB_FORMAT, 1, 0);
                ffi::set_bits(flags, ffi::ConfigAv1::DISABLE_SEQ_HDR, 1, 0);
                ffi::set_bits(flags, ffi::ConfigAv1::REPEAT_SEQ_HDR, 1, 1);
                ffi::set_bits(flags, ffi::ConfigAv1::CHROMA_FORMAT_IDC, 2, 1);
                ffi::set_bits(flags, ffi::ConfigAv1::INPUT_PIXEL_BIT_DEPTH_MINUS8, 3, 0);
                ffi::set_bits(flags, ffi::ConfigAv1::PIXEL_BIT_DEPTH_MINUS8, 3, 0);
                av1.color_primaries = ffi::VUI_BT709;
                av1.transfer_characteristics = ffi::VUI_BT709;
                av1.matrix_coefficients = ffi::VUI_BT709;
                av1.color_range = 0;
            }
        }
    }
}

/// The initialisation parameters for a `width` by `height` session at `settings`,
/// encoding with `config` (which must outlive the call they are passed to).
#[cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "used by the Windows session; built elsewhere for its tests"
    )
)]
pub(crate) fn initialize_params(
    settings: &Settings,
    (width, height): (u32, u32),
    config: *mut Config,
) -> ffi::InitializeParams {
    let mut params = <ffi::InitializeParams as ffi::Zeroed>::zeroed();
    params.version = ffi::INITIALIZE_PARAMS_VER;
    params.encode_guid = guids(settings.format).0;
    params.preset_guid = ffi::PRESET_P5;
    params.encode_width = width;
    params.encode_height = height;
    params.dar_width = width;
    params.dar_height = height;
    params.frame_rate_num = settings.fps.0;
    params.frame_rate_den = settings.fps.1;
    params.enable_encode_async = 1;
    // The encoder decides picture types; a forced IDR asks it for one.
    params.enable_ptd = 1;
    params.encode_config = config;
    params.max_encode_width = width;
    params.max_encode_height = height;
    params.tuning_info = ffi::TUNING_INFO_HIGH_QUALITY;
    params
}

#[cfg(windows)]
mod session {
    use std::ffi::CStr;
    use std::time::{Duration, Instant};

    use jstream_platform::media::{Packet, PixelFormat};
    use jstream_platform::{
        CodecInfo, CodecRole, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface,
    };
    use windows::Win32::Foundation::{FreeLibrary, HMODULE, WAIT_OBJECT_0};
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    use windows::Win32::System::Threading::WaitForSingleObject;
    use windows::core::{Interface, s, w};

    use super::ffi::{self, Encoder, FunctionList, Zeroed};
    use super::{configure, guids, initialize_params};
    use crate::codec::direct::{Ring, Slots};
    use crate::codec::video::{Settings, check};
    use crate::com::Event;
    use crate::gpu::EncodeGpu;

    /// Input textures, and so frames in flight at most.
    const SLOTS: usize = 8;
    /// How long `finish` waits for the encoder to drain.
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
    /// How long dropping an unfinished session waits for the pictures in flight.
    const DROP_TIMEOUT: Duration = Duration::from_secs(1);

    /// `nvEncodeAPI64.dll`, freed on drop.
    struct Library(HMODULE);

    impl Drop for Library {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful LoadLibraryExW and is freed once,
            // after every session object made through it (field order of the encoder).
            let _ = unsafe { FreeLibrary(self.0) };
        }
    }

    /// What one input slot holds beside its texture.
    struct Slot {
        /// The texture, registered with NVENC.
        registered: *mut core::ffi::c_void,
        /// The texture mapped for the picture in flight, until its packet is out.
        mapped: Option<*mut core::ffi::c_void>,
        /// The slot's output buffer.
        bitstream: *mut core::ffi::c_void,
        /// Fired by NVENC when the slot's picture is encoded; registered with it.
        event: Event,
        /// Whether `event` is registered (so unregistered on drop).
        event_registered: bool,
    }

    /// One H.264, HEVC or AV1 session on NVENC, taking the context's encode targets.
    pub(crate) struct NvencEncoder {
        // The session is torn down in `Drop`; the fields then drop in order: the slots'
        // events, the ring's textures, and the library last.
        encoder: Encoder,
        api: Box<FunctionList>,
        slots: Slots,
        inputs: Vec<Slot>,
        /// Fired by NVENC once the end-of-stream picture has drained everything.
        eos: Event,
        eos_registered: bool,
        info: CodecInfo,
        config: VideoEncoderConfig,
        settings: Settings,
        /// A keyframe asked for, set on the next picture.
        force_idr: bool,
        pending: Vec<Packet>,
        finished: bool,
        ring: Ring,
        _library: Library,
    }

    // SAFETY: the session is owned and driven by one thread at a time (`&mut self` for
    // every call that touches NVENC; `can_accept` and `info` read plain fields). An NVENC
    // session and its registered resources, buffers and events may be used from any
    // thread as long as calls don't overlap, which `&mut self` ensures. The D3D11 device
    // is multithread-protected, and `Ring` holds the free-threaded textures and the
    // `Send + Sync` `EncodeGpu`.
    unsafe impl Send for NvencEncoder {}

    /// An `NVENCSTATUS` failure of `call`, with the session's last error message when
    /// there is a session.
    fn failure(
        api: &FunctionList,
        encoder: Encoder,
        call: &'static str,
        status: ffi::Status,
    ) -> PlatformError {
        let detail = match (api.nv_enc_get_last_error_string, encoder.is_null()) {
            (Some(last), false) => {
                // SAFETY: the session is open; the call returns a null-terminated string
                // the driver owns, valid until its next call on the session, and it is
                // copied before that.
                let text = unsafe { last(encoder) };
                if text.is_null() {
                    String::new()
                } else {
                    // SAFETY: as above, a valid null-terminated string.
                    unsafe { CStr::from_ptr(text) }
                        .to_string_lossy()
                        .into_owned()
                }
            }
            _ => String::new(),
        };
        PlatformError::Os {
            call,
            code: Some(status),
            message: if detail.is_empty() {
                ffi::status_name(status).to_owned()
            } else {
                format!("{}: {detail}", ffi::status_name(status))
            },
        }
    }

    /// A function the driver's table lacks.
    fn missing(call: &'static str) -> PlatformError {
        PlatformError::Os {
            call,
            code: None,
            message: "the driver's NVENC function table has no such entry".into(),
        }
    }

    /// Calls `$f` from the table with the session and the rest, mapping a failure.
    macro_rules! nv {
        ($self:expr, $field:ident, $call:literal $(, $arg:expr)* $(,)?) => {{
            let f = $self.api.$field.ok_or_else(|| missing($call))?;
            // SAFETY: (the caller's justification covers the arguments) the table came
            // from NvEncodeAPICreateInstance for API 12.0, so `f` has the declared
            // signature, and the session is open.
            let status = unsafe { f($self.encoder $(, $arg)*) };
            if status == ffi::SUCCESS {
                Ok(())
            } else {
                Err(failure(&$self.api, $self.encoder, $call, status))
            }
        }};
    }

    impl NvencEncoder {
        /// Opens NVENC on `gpu`'s D3D11 device for `config`.
        ///
        /// # Errors
        /// [`PlatformError::Unsupported`] when the configuration isn't one this path
        /// encodes, the driver is missing or older than API 12.0, or the GPU doesn't
        /// offer the codec, asynchronous encoding or the size; [`PlatformError::Os`] when
        /// NVENC refuses a call.
        pub(crate) fn open(
            config: &VideoEncoderConfig,
            gpu: &EncodeGpu,
        ) -> Result<Self, PlatformError> {
            let settings = check(config)?;
            if config.input_format != PixelFormat::Nv12 {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "NVENC from {:?} GPU frames (encode targets are NV12)",
                        config.input_format
                    ),
                });
            }
            let library = load()?;
            let api = function_list(&library)?;
            let mut session = Self {
                encoder: std::ptr::null_mut(),
                api,
                slots: Slots::new(0),
                inputs: Vec::new(),
                eos: Event::new()?,
                eos_registered: false,
                info: CodecInfo {
                    name: format!("NVIDIA NVENC {} (direct)", settings.format.name()),
                    codec: settings.format.codec(),
                    role: CodecRole::VideoEncoder,
                    hardware: true,
                },
                config: *config,
                settings,
                force_idr: false,
                pending: Vec::new(),
                finished: false,
                ring: Ring::new(gpu, config.size, SLOTS)?,
                _library: library,
            };
            session.open_session(gpu)?;
            session.check_support()?;
            session.initialize()?;
            session.make_slots()?;
            Ok(session)
        }

        /// `nvEncOpenEncodeSessionEx` on the context's D3D11 device.
        fn open_session(&mut self, gpu: &EncodeGpu) -> Result<(), PlatformError> {
            let open = self
                .api
                .nv_enc_open_encode_session_ex
                .ok_or_else(|| missing("nvEncOpenEncodeSessionEx"))?;
            let mut params = ffi::OpenEncodeSessionExParams::zeroed();
            params.version = ffi::OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            params.device_type = ffi::DEVICE_TYPE_DIRECTX;
            // An `ID3D11Device5` pointer is an `ID3D11Device` pointer (COM inheritance).
            params.device = gpu.device().as_raw();
            params.api_version = ffi::API_VERSION;
            let mut encoder = std::ptr::null_mut();
            // SAFETY: the table's entry for API 12.0; the parameters are a valid,
            // versioned struct and the device a live D3D11 device, which NVENC takes its
            // own reference to. `encoder` is a valid out-pointer.
            let status = unsafe { open(&mut params, &mut encoder) };
            if status != ffi::SUCCESS {
                return Err(match status {
                    // Not an NVIDIA device NVENC can open: no direct path here.
                    1..=5 => PlatformError::Unsupported {
                        what: format!(
                            "NVENC on the context's device ({})",
                            ffi::status_name(status)
                        ),
                    },
                    _ => failure(
                        &self.api,
                        std::ptr::null_mut(),
                        "nvEncOpenEncodeSessionEx",
                        status,
                    ),
                });
            }
            self.encoder = encoder;
            Ok(())
        }

        /// Checks that the GPU encodes the codec asynchronously at the configured size.
        fn check_support(&self) -> Result<(), PlatformError> {
            let (codec, _) = guids(self.settings.format);
            let name = self.settings.format.name();
            let mut count = 0u32;
            // SAFETY: `count` is a valid out-pointer.
            nv!(
                self,
                nv_enc_get_encode_guid_count,
                "nvEncGetEncodeGUIDCount",
                &mut count
            )?;
            let mut offered = vec![ffi::Guid::default(); count as usize];
            let mut got = 0u32;
            // SAFETY: `offered` holds `count` GUIDs, the size passed; `got` is a valid
            // out-pointer.
            nv!(
                self,
                nv_enc_get_encode_guids,
                "nvEncGetEncodeGUIDs",
                offered.as_mut_ptr(),
                count,
                &mut got
            )?;
            offered.truncate(got as usize);
            if !offered.contains(&codec) {
                return Err(PlatformError::Unsupported {
                    what: format!("{name} on this GPU's NVENC"),
                });
            }
            let cap = |query: u32| -> Result<i32, PlatformError> {
                let mut param = ffi::CapsParam::zeroed();
                param.version = ffi::CAPS_PARAM_VER;
                param.caps_to_query = query;
                let mut value = 0i32;
                // SAFETY: a valid, versioned caps struct and out-pointer.
                nv!(
                    self,
                    nv_enc_get_encode_caps,
                    "nvEncGetEncodeCaps",
                    codec,
                    &mut param,
                    &mut value
                )?;
                Ok(value)
            };
            if cap(ffi::CAPS_ASYNC_ENCODE_SUPPORT)? == 0 {
                return Err(PlatformError::Unsupported {
                    what: format!("asynchronous {name} encoding on this GPU's NVENC"),
                });
            }
            let (w, h) = (self.config.size.width(), self.config.size.height());
            let (max_w, max_h) = (cap(ffi::CAPS_WIDTH_MAX)?, cap(ffi::CAPS_HEIGHT_MAX)?);
            if i64::from(w) > i64::from(max_w) || i64::from(h) > i64::from(max_h) {
                return Err(PlatformError::Unsupported {
                    what: format!("{name} at {w}x{h} on NVENC (at most {max_w}x{max_h})"),
                });
            }
            Ok(())
        }

        /// Reads the P5 high-quality preset, applies [`configure`] and initialises the
        /// encoder with it.
        fn initialize(&mut self) -> Result<(), PlatformError> {
            let (codec, _) = guids(self.settings.format);
            let mut preset = Box::new(ffi::PresetConfig::zeroed());
            preset.version = ffi::PRESET_CONFIG_VER;
            preset.preset_cfg.version = ffi::CONFIG_VER;
            // SAFETY: a valid, versioned preset struct, boxed so it outlives the call.
            nv!(
                self,
                nv_enc_get_encode_preset_config_ex,
                "nvEncGetEncodePresetConfigEx",
                codec,
                ffi::PRESET_P5,
                ffi::TUNING_INFO_HIGH_QUALITY,
                &mut *preset
            )?;
            let mut encode = Box::new(preset.preset_cfg);
            configure(&mut encode, &self.settings, self.config.keyframe_interval);
            let size = (self.config.size.width(), self.config.size.height());
            let mut params = Box::new(initialize_params(&self.settings, size, &mut *encode));
            // SAFETY: valid, versioned parameters whose `encode_config` points to the
            // boxed configuration, live for the call (NVENC copies it).
            nv!(
                self,
                nv_enc_initialize_encoder,
                "nvEncInitializeEncoder",
                &mut *params
            )
        }

        /// Registers each ring texture, and makes its output buffer and its event.
        fn make_slots(&mut self) -> Result<(), PlatformError> {
            let (w, h) = (self.config.size.width(), self.config.size.height());
            self.eos_registered = self.register_event(self.eos.handle().0)?;
            for slot in 0..self.ring.len() {
                let mut entry = Slot {
                    registered: std::ptr::null_mut(),
                    mapped: None,
                    bitstream: std::ptr::null_mut(),
                    event: Event::new()?,
                    event_registered: false,
                };
                let mut reg = ffi::RegisterResource::zeroed();
                reg.version = ffi::REGISTER_RESOURCE_VER;
                reg.resource_type = ffi::INPUT_RESOURCE_TYPE_DIRECTX;
                reg.width = w;
                reg.height = h;
                reg.resource_to_register = self.ring.texture(slot).as_raw();
                reg.buffer_format = ffi::BUFFER_FORMAT_NV12;
                reg.buffer_usage = ffi::INPUT_IMAGE;
                // SAFETY: valid, versioned parameters naming a live NV12 texture on the
                // session's device, which outlives the registration (`Drop` unregisters
                // it before the ring drops).
                nv!(
                    self,
                    nv_enc_register_resource,
                    "nvEncRegisterResource",
                    &mut reg
                )?;
                entry.registered = reg.registered_resource;
                // Pushed now, so a later failure still unregisters it.
                self.inputs.push(entry);
                let mut buffer = ffi::CreateBitstreamBuffer::zeroed();
                buffer.version = ffi::CREATE_BITSTREAM_BUFFER_VER;
                // SAFETY: valid, versioned parameters.
                nv!(
                    self,
                    nv_enc_create_bitstream_buffer,
                    "nvEncCreateBitstreamBuffer",
                    &mut buffer
                )?;
                let handle = self.inputs[slot].event.handle().0;
                let registered = self.register_event(handle)?;
                let entry = &mut self.inputs[slot];
                entry.bitstream = buffer.bitstream_buffer;
                entry.event_registered = registered;
            }
            self.slots = Slots::new(self.inputs.len());
            Ok(())
        }

        /// Registers `event` for completions.
        fn register_event(&self, event: *mut core::ffi::c_void) -> Result<bool, PlatformError> {
            let mut params = ffi::EventParams::zeroed();
            params.version = ffi::EVENT_PARAMS_VER;
            params.completion_event = event;
            // SAFETY: valid, versioned parameters naming a live event, which stays
            // open until it is unregistered in `Drop`.
            nv!(
                self,
                nv_enc_register_async_event,
                "nvEncRegisterAsyncEvent",
                &mut params
            )?;
            Ok(true)
        }

        /// Unregisters `event`, best effort.
        fn unregister_event(&self, event: *mut core::ffi::c_void) {
            let mut params = ffi::EventParams::zeroed();
            params.version = ffi::EVENT_PARAMS_VER;
            params.completion_event = event;
            let unregister: Result<(), PlatformError> =
                // SAFETY: valid, versioned parameters naming an event registered with
                // this session.
                (|| nv!(self, nv_enc_unregister_async_event, "nvEncUnregisterAsyncEvent", &mut params))();
            let _ = unregister;
        }

        /// Unmaps `slot`'s texture if a picture mapped it.
        fn unmap(&mut self, slot: usize) -> Result<(), PlatformError> {
            let Some(mapped) = self.inputs[slot].mapped.take() else {
                return Ok(());
            };
            // SAFETY: `mapped` came from nvEncMapInputResource on this session and is
            // unmapped once.
            nv!(
                self,
                nv_enc_unmap_input_resource,
                "nvEncUnmapInputResource",
                mapped
            )
        }

        /// Takes the packet of the oldest picture in flight, whose event has fired.
        fn collect_oldest(&mut self) -> Result<(), PlatformError> {
            let Some(oldest) = self.slots.oldest() else {
                return Ok(());
            };
            let slot = oldest.slot;
            let mut lock = ffi::LockBitstream::zeroed();
            lock.version = ffi::LOCK_BITSTREAM_VER;
            lock.output_bitstream = self.inputs[slot].bitstream;
            // SAFETY: valid, versioned parameters naming the slot's output buffer, whose
            // picture has completed, so the call doesn't wait.
            nv!(self, nv_enc_lock_bitstream, "nvEncLockBitstream", &mut lock)?;
            let data = if lock.bitstream_buffer_ptr.is_null() {
                Vec::new()
            } else {
                // SAFETY: while locked, the driver's pointer is valid for
                // `bitstream_size_in_bytes` bytes; they are copied before the unlock.
                unsafe {
                    std::slice::from_raw_parts(
                        lock.bitstream_buffer_ptr.cast::<u8>(),
                        lock.bitstream_size_in_bytes as usize,
                    )
                }
                .to_vec()
            };
            let bitstream = self.inputs[slot].bitstream;
            // SAFETY: the buffer was locked above and is unlocked once.
            nv!(
                self,
                nv_enc_unlock_bitstream,
                "nvEncUnlockBitstream",
                bitstream
            )?;
            self.unmap(slot)?;
            // Pictures come out in input order (no B-frames), so this is the oldest's.
            self.slots.complete(oldest.seq);
            let keyframe = matches!(lock.picture_type, ffi::PIC_TYPE_IDR | ffi::PIC_TYPE_I);
            if !data.is_empty() {
                self.pending.push(Packet::new(
                    self.settings.format.codec(),
                    keyframe,
                    oldest.pts,
                    oldest.pts,
                    data,
                )?);
            }
            Ok(())
        }

        /// Whether the oldest picture in flight has completed, waiting up to `ms`.
        fn oldest_done(&self, ms: u32) -> bool {
            self.slots.oldest().is_some_and(|oldest| {
                // SAFETY: the handle is the slot's live event.
                let r = unsafe { WaitForSingleObject(self.inputs[oldest.slot].event.handle(), ms) };
                r == WAIT_OBJECT_0
            })
        }

        /// Sends the end-of-stream picture, which drains the encoder.
        fn send_eos(&mut self) -> Result<(), PlatformError> {
            let mut pic = ffi::PicParams::zeroed();
            pic.version = ffi::PIC_PARAMS_VER;
            pic.encode_pic_flags = ffi::PIC_FLAG_EOS;
            pic.completion_event = self.eos.handle().0;
            // SAFETY: valid, versioned parameters; the event is registered.
            nv!(
                self,
                nv_enc_encode_picture,
                "nvEncEncodePicture (end of stream)",
                &mut pic
            )
        }
    }

    impl VideoEncoder for NvencEncoder {
        fn info(&self) -> &CodecInfo {
            &self.info
        }

        fn can_accept(&self) -> bool {
            !self.finished && self.slots.peek_free().is_some()
        }

        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "video encode",
                    state: "the encoder is finished",
                });
            }
            let info = *frame.info();
            if info.size != self.config.size || info.format != self.config.input_format {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "a {:?} {}x{} frame (configured for {:?} {}x{})",
                        info.format,
                        info.size.width(),
                        info.size.height(),
                        self.config.input_format,
                        self.config.size.width(),
                        self.config.size.height()
                    ),
                });
            }
            let VideoSurface::Gpu(frame) = frame else {
                return Err(PlatformError::Unsupported {
                    what: "a CPU frame on the GPU video encoder path".into(),
                });
            };
            // Collect what is done first, which may free a slot.
            self.poll_ready()?;
            let Some(slot) = self.slots.peek_free() else {
                return Err(PlatformError::Busy {
                    what: "NVENC input textures",
                });
            };
            self.ring.copy(&frame, slot)?;
            // The encode target goes back to its producer now; the copy is queued.
            drop(frame);
            let mut map = ffi::MapInputResource::zeroed();
            map.version = ffi::MAP_INPUT_RESOURCE_VER;
            map.registered_resource = self.inputs[slot].registered;
            // SAFETY: valid, versioned parameters naming the slot's registered texture,
            // which no picture in flight holds (the slot is free).
            nv!(
                self,
                nv_enc_map_input_resource,
                "nvEncMapInputResource",
                &mut map
            )?;
            self.inputs[slot].mapped = Some(map.mapped_resource);
            let sent = self.slots.send(slot, info.pts);
            let mut pic = ffi::PicParams::zeroed();
            pic.version = ffi::PIC_PARAMS_VER;
            pic.input_width = self.config.size.width();
            pic.input_height = self.config.size.height();
            pic.encode_pic_flags = if self.force_idr {
                ffi::PIC_FLAG_FORCEIDR | ffi::PIC_FLAG_OUTPUT_SPSPPS
            } else {
                0
            };
            pic.input_time_stamp = sent.seq;
            pic.input_buffer = map.mapped_resource;
            pic.output_bitstream = self.inputs[slot].bitstream;
            pic.completion_event = self.inputs[slot].event.handle().0;
            pic.buffer_fmt = map.mapped_buffer_fmt;
            pic.picture_struct = ffi::PIC_STRUCT_FRAME;
            let f = self
                .api
                .nv_enc_encode_picture
                .ok_or_else(|| missing("nvEncEncodePicture"))?;
            // SAFETY: valid, versioned parameters naming the slot's mapped input, its
            // output buffer and its registered event, all owned by this session.
            let status = unsafe { f(self.encoder, &mut pic) };
            if status != ffi::SUCCESS && status != ffi::ERR_NEED_MORE_INPUT {
                let err = failure(&self.api, self.encoder, "nvEncEncodePicture", status);
                self.slots.unsend(sent.seq);
                let _ = self.unmap(slot);
                return Err(err);
            }
            self.force_idr = false;
            Ok(())
        }

        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.finished {
                self.poll_ready()?;
            }
            out.append(&mut self.pending);
            Ok(())
        }

        /// `NV_ENC_PIC_FLAG_FORCEIDR` with the parameter sets on the next picture.
        fn request_keyframe(&mut self) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "video keyframe request",
                    state: "the encoder is finished",
                });
            }
            self.force_idr = true;
            Ok(())
        }

        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.finished {
                self.finished = true;
                self.send_eos()?;
                let deadline = Instant::now() + DRAIN_TIMEOUT;
                while self.slots.in_flight() > 0 {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if !self.oldest_done(u32::try_from(left.as_millis()).unwrap_or(u32::MAX)) {
                        out.append(&mut self.pending);
                        return Err(PlatformError::Os {
                            call: "nvEncEncodePicture (end of stream)",
                            code: None,
                            message: format!(
                                "the encoder didn't drain within {} s ({} frames still in flight)",
                                DRAIN_TIMEOUT.as_secs(),
                                self.slots.in_flight()
                            ),
                        });
                    }
                    self.collect_oldest()?;
                }
                // SAFETY: the handle is the live end-of-stream event.
                let _ = unsafe { WaitForSingleObject(self.eos.handle(), 100) };
            }
            out.append(&mut self.pending);
            Ok(())
        }
    }

    impl NvencEncoder {
        /// Takes every packet that is ready, oldest first, without waiting.
        fn poll_ready(&mut self) -> Result<(), PlatformError> {
            while self.oldest_done(0) {
                self.collect_oldest()?;
            }
            Ok(())
        }
    }

    impl Drop for NvencEncoder {
        fn drop(&mut self) {
            if self.encoder.is_null() {
                return;
            }
            // Pictures still in flight: drain them, best effort, before releasing what
            // they use.
            if !self.finished && self.slots.in_flight() > 0 && self.send_eos().is_ok() {
                let ms = u32::try_from(DROP_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
                // SAFETY: the handle is the live end-of-stream event.
                let _ = unsafe { WaitForSingleObject(self.eos.handle(), ms) };
            }
            for slot in 0..self.inputs.len() {
                let _ = self.unmap(slot);
                let (registered, bitstream) =
                    (self.inputs[slot].registered, self.inputs[slot].bitstream);
                if !registered.is_null() {
                    let unregister: Result<(), PlatformError> =
                        // SAFETY: registered with this session and unregistered once.
                        (|| nv!(self, nv_enc_unregister_resource, "nvEncUnregisterResource", registered))();
                    let _ = unregister;
                }
                if !bitstream.is_null() {
                    let destroy: Result<(), PlatformError> =
                        // SAFETY: made by this session and destroyed once.
                        (|| nv!(self, nv_enc_destroy_bitstream_buffer, "nvEncDestroyBitstreamBuffer", bitstream))();
                    let _ = destroy;
                }
                if self.inputs[slot].event_registered {
                    self.unregister_event(self.inputs[slot].event.handle().0);
                }
            }
            if self.eos_registered {
                self.unregister_event(self.eos.handle().0);
            }
            if let Some(destroy) = self.api.nv_enc_destroy_encoder {
                // SAFETY: the session is open, everything made through it is released
                // above, and it is destroyed once.
                let _ = unsafe { destroy(self.encoder) };
            }
            self.encoder = std::ptr::null_mut();
        }
    }

    /// Loads the driver's `nvEncodeAPI64.dll` from System32.
    fn load() -> Result<Library, PlatformError> {
        // SAFETY: plain call with a static, null-terminated name. Only System32 is
        // searched, where the NVIDIA driver installs the library; its initialisation has
        // no requirements on the caller.
        let module =
            unsafe { LoadLibraryExW(w!("nvEncodeAPI64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .map_err(|e| PlatformError::Unsupported {
                    what: format!("NVENC (the NVIDIA driver's nvEncodeAPI64.dll didn't load: {e})"),
                })?;
        Ok(Library(module))
    }

    /// Checks the driver's API version and fills the function table.
    fn function_list(library: &Library) -> Result<Box<FunctionList>, PlatformError> {
        // SAFETY: the module is loaded for as long as `library` lives; the names are
        // static and null-terminated.
        let (max, create) = unsafe {
            (
                GetProcAddress(library.0, s!("NvEncodeAPIGetMaxSupportedVersion")),
                GetProcAddress(library.0, s!("NvEncodeAPICreateInstance")),
            )
        };
        let (Some(max), Some(create)) = (max, create) else {
            return Err(PlatformError::Unsupported {
                what: "NVENC (nvEncodeAPI64.dll lacks its entry points)".into(),
            });
        };
        // SAFETY: the exports have these signatures in every NVENC driver (the
        // header's `NvEncodeAPIGetMaxSupportedVersion` and `NvEncodeAPICreateInstance`).
        let (max, create) = unsafe {
            (
                std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    ffi::GetMaxSupportedVersion,
                >(max),
                std::mem::transmute::<unsafe extern "system" fn() -> isize, ffi::CreateInstance>(
                    create,
                ),
            )
        };
        let mut version = 0u32;
        // SAFETY: a valid out-pointer.
        let status = unsafe { max(&mut version) };
        if status != ffi::SUCCESS {
            return Err(failure(
                &FunctionList::zeroed(),
                std::ptr::null_mut(),
                "NvEncodeAPIGetMaxSupportedVersion",
                status,
            ));
        }
        if version < ffi::MIN_SUPPORTED_VERSION {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "NVENC API 12.0 (the driver offers {}.{}; update the NVIDIA driver to \
                     522 or later)",
                    version >> 4,
                    version & 0xF
                ),
            });
        }
        let mut list = Box::new(FunctionList::zeroed());
        list.version = ffi::FUNCTION_LIST_VER;
        // SAFETY: a valid, versioned function table the call fills in.
        let status = unsafe { create(&mut *list) };
        if status != ffi::SUCCESS {
            return Err(failure(
                &list,
                std::ptr::null_mut(),
                "NvEncodeAPICreateInstance",
                status,
            ));
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{Codec, FrameSize, PixelFormat, Timebase};
    use jstream_platform::{HardwarePreference, RateControl, VideoEncoderConfig};

    use super::ffi::{self, Zeroed, bits};
    use super::{configure, guids, initialize_params};
    use crate::codec::video::{Format, check};

    fn settings(codec: Codec, rate_control: RateControl) -> crate::codec::video::Settings {
        check(&VideoEncoderConfig {
            codec,
            size: FrameSize::new(1920, 1080).expect("size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1001, 30_000).expect("29.97"),
            rate_control,
            keyframe_interval: 60,
            hardware: HardwarePreference::Require,
        })
        .expect("settings")
    }

    /// A preset's configuration with every bit set, so the test sees which ones
    /// `configure` clears.
    fn preset() -> ffi::Config {
        let mut config = ffi::Config::zeroed();
        config.frame_interval_p = 3;
        config.rc_params.flags = u32::MAX;
        config.rc_params.lookahead_depth = 32;
        config
    }

    #[test]
    fn h264_is_cbr_high_with_parameter_sets_on_every_idr() {
        let s = settings(Codec::H264, RateControl::Cbr { kbps: 6000 });
        let mut config = preset();
        // Writing a union member is safe; reading it back below is not.
        config.codec.h264.flags = 1 << ffi::ConfigH264::DISABLE_SPSPPS;
        configure(&mut config, &s, 60);
        assert_eq!(config.version, ffi::CONFIG_VER);
        assert_eq!(config.profile_guid, ffi::H264_PROFILE_HIGH);
        assert_eq!(config.gop_length, 60);
        assert_eq!(config.frame_interval_p, 1, "no B-frames");
        let rc = config.rc_params;
        assert_eq!(rc.rate_control_mode, ffi::RC_CBR);
        assert_eq!(
            (rc.average_bit_rate, rc.max_bit_rate),
            (6_000_000, 6_000_000)
        );
        assert_eq!(rc.vbv_buffer_size, 6_000_000, "one second");
        assert_eq!(bits(rc.flags, ffi::RcParams::LOOKAHEAD, 1), 0);
        assert_eq!(bits(rc.flags, ffi::RcParams::ZERO_REORDER_DELAY, 1), 1);
        assert_eq!(rc.lookahead_depth, 0);
        // SAFETY: test data; the union's members are plain integers.
        let h264 = unsafe { config.codec.h264 };
        assert_eq!(h264.idr_period, 60);
        assert_eq!(bits(h264.flags, ffi::ConfigH264::REPEAT_SPSPPS, 1), 1);
        assert_eq!(bits(h264.flags, ffi::ConfigH264::DISABLE_SPSPPS, 1), 0);
        assert_eq!(h264.chroma_format_idc, 1);
        assert_eq!(h264.vui.video_full_range_flag, 0, "limited range");
        assert_eq!(h264.vui.colour_description_present_flag, 1);
        assert_eq!(
            (
                h264.vui.colour_primaries,
                h264.vui.transfer_characteristics,
                h264.vui.colour_matrix
            ),
            (1, 1, 1),
            "BT.709"
        );
    }

    #[test]
    fn hevc_is_main_8_bit_420() {
        let s = settings(
            Codec::Hevc,
            RateControl::Vbr {
                kbps: 4000,
                max_kbps: 8000,
            },
        );
        let mut config = preset();
        configure(&mut config, &s, 120);
        assert_eq!(config.profile_guid, ffi::HEVC_PROFILE_MAIN);
        let rc = config.rc_params;
        assert_eq!(rc.rate_control_mode, ffi::RC_VBR);
        assert_eq!(
            (rc.average_bit_rate, rc.max_bit_rate),
            (4_000_000, 8_000_000)
        );
        assert_eq!(rc.vbv_buffer_size, 8_000_000);
        // SAFETY: test data; the union's members are plain integers.
        let hevc = unsafe { config.codec.hevc };
        assert_eq!(hevc.idr_period, 120);
        assert_eq!(bits(hevc.flags, ffi::ConfigHevc::REPEAT_SPSPPS, 1), 1);
        assert_eq!(bits(hevc.flags, ffi::ConfigHevc::CHROMA_FORMAT_IDC, 2), 1);
        assert_eq!(
            bits(hevc.flags, ffi::ConfigHevc::PIXEL_BIT_DEPTH_MINUS8, 3),
            0
        );
        assert_eq!(hevc.vui.colour_matrix, ffi::VUI_BT709);
    }

    #[test]
    fn av1_is_low_overhead_obus_with_the_sequence_header_on_every_keyframe() {
        let s = settings(Codec::Av1, RateControl::Cbr { kbps: 5000 });
        let mut config = preset();
        config.codec.av1.flags = u32::MAX;
        configure(&mut config, &s, 60);
        assert_eq!(config.profile_guid, ffi::AV1_PROFILE_MAIN);
        // SAFETY: test data; the union's members are plain integers.
        let av1 = unsafe { config.codec.av1 };
        assert_eq!(av1.idr_period, 60);
        assert_eq!(bits(av1.flags, ffi::ConfigAv1::OUTPUT_ANNEXB_FORMAT, 1), 0);
        assert_eq!(bits(av1.flags, ffi::ConfigAv1::DISABLE_SEQ_HDR, 1), 0);
        assert_eq!(bits(av1.flags, ffi::ConfigAv1::REPEAT_SEQ_HDR, 1), 1);
        assert_eq!(bits(av1.flags, ffi::ConfigAv1::CHROMA_FORMAT_IDC, 2), 1);
        assert_eq!(
            bits(av1.flags, ffi::ConfigAv1::INPUT_PIXEL_BIT_DEPTH_MINUS8, 3),
            0
        );
        assert_eq!(
            bits(av1.flags, ffi::ConfigAv1::PIXEL_BIT_DEPTH_MINUS8, 3),
            0
        );
        // Bits it doesn't own are left as the preset had them.
        assert_eq!(bits(av1.flags, 9, 3), 0b111);
        assert_eq!(
            (
                av1.color_primaries,
                av1.transfer_characteristics,
                av1.matrix_coefficients,
                av1.color_range
            ),
            (1, 1, 1, 0)
        );
    }

    #[test]
    fn initialisation_is_asynchronous_at_the_frame_rate() {
        let s = settings(Codec::H264, RateControl::Cbr { kbps: 6000 });
        let mut config = preset();
        let params = initialize_params(&s, (1920, 1080), &mut config);
        assert_eq!(params.version, ffi::INITIALIZE_PARAMS_VER);
        assert_eq!(params.encode_guid, guids(Format::H264).0);
        assert_eq!(params.preset_guid, ffi::PRESET_P5);
        assert_eq!(params.tuning_info, ffi::TUNING_INFO_HIGH_QUALITY);
        assert_eq!((params.encode_width, params.encode_height), (1920, 1080));
        assert_eq!(
            (params.frame_rate_num, params.frame_rate_den),
            (30_000, 1001)
        );
        assert_eq!((params.enable_encode_async, params.enable_ptd), (1, 1));
        assert_eq!(params.encode_config, &mut config as *mut _);
    }

    #[test]
    fn each_format_has_its_codec_and_profile() {
        assert_eq!(
            guids(Format::H264),
            (ffi::CODEC_H264, ffi::H264_PROFILE_HIGH)
        );
        assert_eq!(
            guids(Format::Hevc),
            (ffi::CODEC_HEVC, ffi::HEVC_PROFILE_MAIN)
        );
        assert_eq!(guids(Format::Av1), (ffi::CODEC_AV1, ffi::AV1_PROFILE_MAIN));
    }
}
