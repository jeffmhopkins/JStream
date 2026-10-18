//! ALSA audio input and output, the fallback where no PipeWire daemon runs (ADR-0060 §4,
//! §8).
//!
//! [`AlsaAudio`] is an `AudioIo` backend on libasound, loaded at run time. It lists the
//! PCMs ALSA's configuration names, and records from or plays to one of them, each
//! stream on its own thread. ALSA has no loopback, so desktop and application audio
//! report `Unsupported`, as ADR-0060 §4 says; they come with PipeWire.
//!
//! **Format.** A stream takes the device's own channel count and rate: it asks for
//! stereo at 48 kHz, takes the nearest the device offers, and turns ALSA's own
//! resampling off (`snd_pcm_hw_params_set_rate_resample`), so a `hw:` device runs at a
//! rate it has. A plugin PCM such as `default` may still convert behind ALSA's API, as
//! the sound server under it decides. Samples are `FLOAT_LE` when the device takes it,
//! else `S32_LE` or `S16_LE`, converted to and from `f32` here. A channel count other
//! than 1, 2 or 6 is `Unsupported`, since `ChannelLayout` has nothing else yet.
//!
//! **Timing.** Streams are non-blocking: the thread waits on the PCM for at most
//! [`WAIT_MS`], so dropping a stream stops it within that time even if the device stalls.
//! A packet's time is the media clock (`CLOCK_MONOTONIC`) when it is read, less the
//! frames still queued (`snd_pcm_delay`) and the packet's own frames; a render buffer's is
//! the clock plus the frames queued ahead of it. An overrun or underrun is recovered with
//! `snd_pcm_recover` (a capture stream is then started again, since recovery only
//! prepares it) and the next packet is flagged as a discontinuity. ALSA doesn't say
//! how many frames were lost, so the device position doesn't jump. A device that can't be
//! recovered, such as one unplugged, ends the stream with `DeviceLost`.
//!
//! The pieces that don't touch libasound (sample conversion, channel layouts, which
//! names are listed, frame-to-time arithmetic and error mapping) are platform independent
//! and tested everywhere.

use jstream_platform::PlatformError;
use jstream_platform::media::ChannelLayout;

/// How long a stream's thread waits on the PCM before checking whether it should stop.
#[cfg(target_os = "linux")]
pub(crate) const WAIT_MS: i32 = 100;

/// The period a stream asks for, in microseconds: 10 ms.
#[cfg(target_os = "linux")]
const PERIOD_US: u32 = 10_000;

/// The buffer a stream asks for, in microseconds: four periods.
#[cfg(target_os = "linux")]
const BUFFER_US: u32 = 4 * PERIOD_US;

/// The rate a stream asks for before taking the device's nearest.
#[cfg(target_os = "linux")]
const PREFERRED_RATE: u32 = 48_000;

/// The channel count a stream asks for before taking the device's nearest.
#[cfg(target_os = "linux")]
const PREFERRED_CHANNELS: u32 = 2;

/// 100 ns ticks per second, the media clock's unit.
const HNS_PER_SEC: i64 = 10_000_000;

/// The sample types a stream accepts, in order of preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SampleKind {
    /// `SND_PCM_FORMAT_FLOAT_LE`.
    F32,
    /// `SND_PCM_FORMAT_S32_LE`.
    S32,
    /// `SND_PCM_FORMAT_S16_LE`.
    S16,
}

impl SampleKind {
    /// Every kind, best first.
    pub(crate) const PREFERRED: [SampleKind; 3] =
        [SampleKind::F32, SampleKind::S32, SampleKind::S16];

    /// The `snd_pcm_format_t` value.
    pub(crate) fn alsa_format(self) -> i32 {
        match self {
            SampleKind::F32 => 14,
            SampleKind::S32 => 10,
            SampleKind::S16 => 2,
        }
    }

    /// Bytes per sample.
    pub(crate) fn bytes(self) -> usize {
        match self {
            SampleKind::F32 | SampleKind::S32 => 4,
            SampleKind::S16 => 2,
        }
    }
}

/// Converts little-endian samples of `kind` in `raw` to `f32` in `out`, one for one, as
/// far as the shorter of the two goes. Integers map `-2^(n-1)..2^(n-1)` onto `-1.0..1.0`.
pub(crate) fn to_f32(kind: SampleKind, raw: &[u8], out: &mut [f32]) {
    match kind {
        SampleKind::F32 => {
            for (bytes, sample) in raw.chunks_exact(4).zip(out.iter_mut()) {
                *sample = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            }
        }
        SampleKind::S32 => {
            for (bytes, sample) in raw.chunks_exact(4).zip(out.iter_mut()) {
                let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                // Through f64, so the low bits of a 32-bit sample survive the scaling.
                let scaled = (f64::from(v) / 2_147_483_648.0) as f32;
                *sample = scaled;
            }
        }
        SampleKind::S16 => {
            for (bytes, sample) in raw.chunks_exact(2).zip(out.iter_mut()) {
                let v = i16::from_le_bytes([bytes[0], bytes[1]]);
                *sample = f32::from(v) / 32_768.0;
            }
        }
    }
}

/// Converts `f32` samples to little-endian samples of `kind` in `raw`, one for one, as
/// far as the shorter of the two goes. Integer kinds clamp to `-1.0..=1.0` first, and NaN
/// becomes silence.
pub(crate) fn from_f32(kind: SampleKind, input: &[f32], raw: &mut [u8]) {
    match kind {
        SampleKind::F32 => {
            for (sample, bytes) in input.iter().zip(raw.chunks_exact_mut(4)) {
                bytes.copy_from_slice(&sample.to_le_bytes());
            }
        }
        SampleKind::S32 => {
            for (sample, bytes) in input.iter().zip(raw.chunks_exact_mut(4)) {
                let v = quantize(*sample, 2_147_483_647.0);
                let v = v as i32;
                bytes.copy_from_slice(&v.to_le_bytes());
            }
        }
        SampleKind::S16 => {
            for (sample, bytes) in input.iter().zip(raw.chunks_exact_mut(2)) {
                let v = quantize(*sample, 32_767.0);
                let v = v as i16;
                bytes.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// `sample` clamped to `-1.0..=1.0` and scaled by `full`, rounded; NaN is 0.
fn quantize(sample: f32, full: f64) -> f64 {
    if sample.is_nan() {
        return 0.0;
    }
    (f64::from(sample).clamp(-1.0, 1.0) * full).round()
}

/// The layout for a channel count, if `ChannelLayout` has one.
pub(crate) fn layout_for(channels: u32) -> Option<ChannelLayout> {
    match channels {
        1 => Some(ChannelLayout::Mono),
        2 => Some(ChannelLayout::Stereo),
        6 => Some(ChannelLayout::Surround51),
        _ => None,
    }
}

/// Whether a PCM name from ALSA's hints is listed as a device. `null` discards
/// everything, and the speaker-position and digital-output aliases (`front:`,
/// `surround51:`, `iec958:` and the like) are other views of a card ALSA lists anyway
/// (`hw:`, `plughw:`, `sysdefault:`). They can still be opened by name.
pub(crate) fn listed(name: &str) -> bool {
    const ALIASES: [&str; 6] = ["front", "rear", "center_lfe", "side", "surround", "iec958"];
    if name == "null" || name.is_empty() {
        return false;
    }
    let base = name.split(':').next().unwrap_or(name);
    !ALIASES.iter().any(|alias| base.starts_with(alias))
}

/// A hint's description as a device name: its lines joined with ", ", or `id` when it
/// has none.
pub(crate) fn device_name(description: Option<&str>, id: &str) -> String {
    match description {
        Some(text) if !text.trim().is_empty() => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        _ => id.to_owned(),
    }
}

/// `frames` at `rate` as 100 ns ticks, rounded toward zero; 0 for a zero rate.
pub(crate) fn hns_for_frames(frames: i64, rate: u32) -> i64 {
    if rate == 0 {
        return 0;
    }
    let v = i128::from(frames) * i128::from(HNS_PER_SEC) / i128::from(rate);
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

/// The error for a failed ALSA call on device `id`: a missing device is `NotFound`,
/// anything else `Os` with ALSA's own message.
pub(crate) fn alsa_error(
    call: &'static str,
    code: i32,
    message: String,
    id: &str,
) -> PlatformError {
    let errno = code.unsigned_abs();
    // ENOENT, ENXIO and ENODEV: no such PCM, card or device.
    if matches!(errno, 2 | 6 | 19) {
        return PlatformError::NotFound {
            kind: "audio device",
            id: id.to_owned(),
        };
    }
    PlatformError::Os {
        call,
        code: Some(errno),
        message,
    }
}

#[cfg(target_os = "linux")]
pub use backend::AlsaAudio;

#[cfg(target_os = "linux")]
mod backend {
    use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;

    use jstream_platform::media::{AudioFormat, MediaTime, SampleFormat};
    use jstream_platform::{
        AudioDeviceInfo, AudioDirection, AudioInputHandler, AudioInputTarget, AudioIo,
        AudioOutputHandler, AudioOutputTarget, AudioPacket, AudioPacketFlags, AudioProcess,
        AudioRenderBuffer, AudioStream, DeviceId, MediaClock, PlatformError,
    };

    use super::{
        BUFFER_US, PERIOD_US, PREFERRED_CHANNELS, PREFERRED_RATE, SampleKind, WAIT_MS, alsa_error,
        device_name, from_f32, hns_for_frames, layout_for, listed, to_f32,
    };
    use crate::clock::MonotonicClock;
    use crate::dl::SystemLibrary;

    /// `SND_PCM_STREAM_PLAYBACK`.
    const STREAM_PLAYBACK: c_int = 0;
    /// `SND_PCM_STREAM_CAPTURE`.
    const STREAM_CAPTURE: c_int = 1;
    /// `SND_PCM_NONBLOCK`.
    const NONBLOCK: c_int = 1;
    /// `SND_PCM_ACCESS_RW_INTERLEAVED`.
    const ACCESS_RW_INTERLEAVED: c_int = 3;
    /// `-EAGAIN`: nothing to read or no room to write yet.
    const EAGAIN: c_long = -11;
    /// `SND_PCM_STATE_RUNNING`.
    const STATE_RUNNING: c_int = 3;

    // The libasound functions the backend calls, as declared in <alsa/pcm.h>,
    // <alsa/control.h> (the name hints) and <alsa/error.h>. Opaque handles are `void *`.
    type PcmOpen = unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
    type PcmClose = unsafe extern "C" fn(*mut c_void) -> c_int;
    type PcmOnly = unsafe extern "C" fn(*mut c_void) -> c_int;
    type HwMalloc = unsafe extern "C" fn(*mut *mut c_void) -> c_int;
    type HwFree = unsafe extern "C" fn(*mut c_void);
    type HwAny = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
    type HwSetInt = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int;
    type HwSetUint = unsafe extern "C" fn(*mut c_void, *mut c_void, c_uint) -> c_int;
    type HwNearUint = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_uint) -> c_int;
    type HwNearUintDir =
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_uint, *mut c_int) -> c_int;
    type HwGetPeriodSize = unsafe extern "C" fn(*const c_void, *mut c_ulong, *mut c_int) -> c_int;
    type PcmRead = unsafe extern "C" fn(*mut c_void, *mut c_void, c_ulong) -> c_long;
    type PcmWrite = unsafe extern "C" fn(*mut c_void, *const c_void, c_ulong) -> c_long;
    type PcmRecover = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
    type PcmDelay = unsafe extern "C" fn(*mut c_void, *mut c_long) -> c_int;
    type PcmWait = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type PcmAvail = unsafe extern "C" fn(*mut c_void) -> c_long;
    type PcmState = unsafe extern "C" fn(*mut c_void) -> c_int;
    type StrError = unsafe extern "C" fn(c_int) -> *const c_char;
    type NameHint = unsafe extern "C" fn(c_int, *const c_char, *mut *mut *mut c_void) -> c_int;
    type NameGetHint = unsafe extern "C" fn(*const c_void, *const c_char) -> *mut c_char;
    type NameFreeHint = unsafe extern "C" fn(*mut *mut c_void) -> c_int;

    /// libasound's functions, resolved once.
    struct Api {
        // Keeps the library loaded while the pointers below can be called.
        _lib: SystemLibrary,
        open: PcmOpen,
        close: PcmClose,
        start: PcmOnly,
        hw_malloc: HwMalloc,
        hw_free: HwFree,
        hw_any: HwAny,
        hw_set_access: HwSetInt,
        hw_test_format: HwSetInt,
        hw_set_format: HwSetInt,
        hw_set_rate_resample: HwSetUint,
        hw_set_channels_near: HwNearUint,
        hw_set_rate_near: HwNearUintDir,
        hw_set_buffer_time_near: HwNearUintDir,
        hw_set_period_time_near: HwNearUintDir,
        hw_params: HwAny,
        hw_get_period_size: HwGetPeriodSize,
        readi: PcmRead,
        writei: PcmWrite,
        recover: PcmRecover,
        delay: PcmDelay,
        wait: PcmWait,
        avail_update: PcmAvail,
        state: PcmState,
        strerror: StrError,
        name_hint: NameHint,
        name_get_hint: NameGetHint,
        name_free_hint: NameFreeHint,
    }

    impl std::fmt::Debug for Api {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Api").finish_non_exhaustive()
        }
    }

    impl Api {
        fn load() -> Result<Self, PlatformError> {
            let lib = SystemLibrary::open(
                "ALSA",
                &["libasound.so.2"],
                "install ALSA's library (Debian and Ubuntu: libasound2t64 or libasound2; \
                 Fedora: alsa-lib)",
            )?;
            // SAFETY: for every `function` call below, the type alias above matches the
            // symbol's declaration in alsa-lib's headers (alsa/pcm.h, alsa/control.h,
            // alsa/error.h), with `snd_pcm_t *`, `snd_pcm_hw_params_t *` and the hint
            // array as `void` pointers, `snd_pcm_uframes_t` as `unsigned long` and
            // `snd_pcm_sframes_t` as `long`, as alsa-lib defines them. `lib` is kept in
            // `Api` beside the pointers.
            unsafe {
                Ok(Self {
                    open: lib.function("snd_pcm_open")?,
                    close: lib.function("snd_pcm_close")?,
                    start: lib.function("snd_pcm_start")?,
                    hw_malloc: lib.function("snd_pcm_hw_params_malloc")?,
                    hw_free: lib.function("snd_pcm_hw_params_free")?,
                    hw_any: lib.function("snd_pcm_hw_params_any")?,
                    hw_set_access: lib.function("snd_pcm_hw_params_set_access")?,
                    hw_test_format: lib.function("snd_pcm_hw_params_test_format")?,
                    hw_set_format: lib.function("snd_pcm_hw_params_set_format")?,
                    hw_set_rate_resample: lib.function("snd_pcm_hw_params_set_rate_resample")?,
                    hw_set_channels_near: lib.function("snd_pcm_hw_params_set_channels_near")?,
                    hw_set_rate_near: lib.function("snd_pcm_hw_params_set_rate_near")?,
                    hw_set_buffer_time_near: lib
                        .function("snd_pcm_hw_params_set_buffer_time_near")?,
                    hw_set_period_time_near: lib
                        .function("snd_pcm_hw_params_set_period_time_near")?,
                    hw_params: lib.function("snd_pcm_hw_params")?,
                    hw_get_period_size: lib.function("snd_pcm_hw_params_get_period_size")?,
                    readi: lib.function("snd_pcm_readi")?,
                    writei: lib.function("snd_pcm_writei")?,
                    recover: lib.function("snd_pcm_recover")?,
                    delay: lib.function("snd_pcm_delay")?,
                    wait: lib.function("snd_pcm_wait")?,
                    avail_update: lib.function("snd_pcm_avail_update")?,
                    state: lib.function("snd_pcm_state")?,
                    strerror: lib.function("snd_strerror")?,
                    name_hint: lib.function("snd_device_name_hint")?,
                    name_get_hint: lib.function("snd_device_name_get_hint")?,
                    name_free_hint: lib.function("snd_device_name_free_hint")?,
                    _lib: lib,
                })
            }
        }

        /// ALSA's message for an error code.
        fn message(&self, code: c_int) -> String {
            // SAFETY: `snd_strerror` takes any int and returns a static NUL-terminated
            // string, or null, which is checked.
            let ptr = unsafe { (self.strerror)(code) };
            if ptr.is_null() {
                return format!("ALSA error {code}");
            }
            // SAFETY: non-null, NUL-terminated and static.
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        }

        fn error(&self, call: &'static str, code: c_int, id: &str) -> PlatformError {
            alsa_error(call, code, self.message(code), id)
        }
    }

    /// An open PCM, closed on drop.
    struct Pcm {
        api: Arc<Api>,
        handle: *mut c_void,
        id: String,
    }

    // SAFETY: an alsa-lib PCM handle has no thread affinity; it must only not be used
    // from two threads at once. A `Pcm` is used by one thread at a time: the caller's
    // while it is configured, then only the stream thread it is moved to.
    unsafe impl Send for Pcm {}

    impl Drop for Pcm {
        fn drop(&mut self) {
            // SAFETY: `handle` came from a successful `snd_pcm_open` and is closed once.
            unsafe { (self.api.close)(self.handle) };
        }
    }

    /// The parameters a PCM was configured with.
    #[derive(Clone, Copy, Debug)]
    struct Negotiated {
        format: AudioFormat,
        kind: SampleKind,
        channels: usize,
        rate: u32,
        period: usize,
    }

    /// `snd_pcm_hw_params_t`, freed on drop.
    struct HwParams<'a> {
        api: &'a Api,
        ptr: *mut c_void,
    }

    impl Drop for HwParams<'_> {
        fn drop(&mut self) {
            // SAFETY: `ptr` came from `snd_pcm_hw_params_malloc` and is freed once.
            unsafe { (self.api.hw_free)(self.ptr) };
        }
    }

    impl Pcm {
        /// Opens `id` for `stream`, non-blocking.
        fn open(api: &Arc<Api>, id: &str, stream: c_int) -> Result<Pcm, PlatformError> {
            let name = CString::new(id).map_err(|_| PlatformError::NotFound {
                kind: "audio device",
                id: id.to_owned(),
            })?;
            let mut handle: *mut c_void = std::ptr::null_mut();
            // SAFETY: `handle` is a valid out pointer and `name` a NUL-terminated string,
            // both alive for the call.
            let rc = unsafe { (api.open)(&raw mut handle, name.as_ptr(), stream, NONBLOCK) };
            if rc < 0 || handle.is_null() {
                return Err(api.error("snd_pcm_open", rc, id));
            }
            Ok(Pcm {
                api: Arc::clone(api),
                handle,
                id: id.to_owned(),
            })
        }

        fn check(&self, call: &'static str, rc: c_int) -> Result<(), PlatformError> {
            if rc < 0 {
                Err(self.api.error(call, rc, &self.id))
            } else {
                Ok(())
            }
        }

        /// Sets interleaved access, the best sample kind the device takes, its nearest
        /// channel count and rate with ALSA's resampling off, and a 10 ms period.
        fn configure(&self) -> Result<Negotiated, PlatformError> {
            let api = &*self.api;
            let mut ptr: *mut c_void = std::ptr::null_mut();
            // SAFETY: `ptr` is a valid out pointer.
            let rc = unsafe { (api.hw_malloc)(&raw mut ptr) };
            if rc < 0 || ptr.is_null() {
                return Err(api.error("snd_pcm_hw_params_malloc", rc, &self.id));
            }
            let params = HwParams { api, ptr };
            // Every call below passes `pcm` and `hw`, live handles from `snd_pcm_open`
            // and `snd_pcm_hw_params_malloc`, and out pointers to locals that outlive it.
            let (pcm, hw) = (self.handle, params.ptr);
            // SAFETY: live handles (above).
            let rc = unsafe { (api.hw_any)(pcm, hw) };
            self.check("snd_pcm_hw_params_any", rc)?;
            // SAFETY: live handles (above).
            let rc = unsafe { (api.hw_set_access)(pcm, hw, ACCESS_RW_INTERLEAVED) };
            self.check("snd_pcm_hw_params_set_access", rc)?;
            let takes = |kind: SampleKind| {
                // SAFETY: live handles (above); testing changes nothing.
                unsafe { (api.hw_test_format)(pcm, hw, kind.alsa_format()) == 0 }
            };
            let kind = SampleKind::PREFERRED
                .into_iter()
                .find(|kind| takes(*kind))
                .ok_or_else(|| PlatformError::Unsupported {
                    what: format!(
                        "ALSA device {:?}: none of the sample formats FLOAT_LE, S32_LE, S16_LE",
                        self.id
                    ),
                })?;
            // SAFETY: live handles (above).
            let rc = unsafe { (api.hw_set_format)(pcm, hw, kind.alsa_format()) };
            self.check("snd_pcm_hw_params_set_format", rc)?;
            // Best effort: a PCM that can't switch resampling off keeps what it does.
            // SAFETY: live handles (above).
            let _ = unsafe { (api.hw_set_rate_resample)(pcm, hw, 0) };
            let mut channels: c_uint = PREFERRED_CHANNELS;
            // SAFETY: live handles and a local out pointer (above).
            let rc = unsafe { (api.hw_set_channels_near)(pcm, hw, &raw mut channels) };
            self.check("snd_pcm_hw_params_set_channels_near", rc)?;
            let layout = layout_for(channels).ok_or_else(|| PlatformError::Unsupported {
                what: format!("ALSA device {:?} with {channels} channels", self.id),
            })?;
            let mut rate: c_uint = PREFERRED_RATE;
            let mut dir: c_int = 0;
            // SAFETY: live handles and local out pointers (above).
            let rc = unsafe { (api.hw_set_rate_near)(pcm, hw, &raw mut rate, &raw mut dir) };
            self.check("snd_pcm_hw_params_set_rate_near", rc)?;
            let mut buffer_us: c_uint = BUFFER_US;
            // `dir` is an input too: start each call from "exactly", not the last
            // call's answer.
            dir = 0;
            // SAFETY: live handles and local out pointers (above).
            let rc =
                unsafe { (api.hw_set_buffer_time_near)(pcm, hw, &raw mut buffer_us, &raw mut dir) };
            self.check("snd_pcm_hw_params_set_buffer_time_near", rc)?;
            let mut period_us: c_uint = PERIOD_US;
            dir = 0;
            // SAFETY: live handles and local out pointers (above).
            let rc =
                unsafe { (api.hw_set_period_time_near)(pcm, hw, &raw mut period_us, &raw mut dir) };
            self.check("snd_pcm_hw_params_set_period_time_near", rc)?;
            // SAFETY: live handles (above).
            let rc = unsafe { (api.hw_params)(pcm, hw) };
            self.check("snd_pcm_hw_params", rc)?;
            let mut period: c_ulong = 0;
            // SAFETY: a live parameter block and local out pointers (above).
            let rc = unsafe { (api.hw_get_period_size)(hw, &raw mut period, &raw mut dir) };
            self.check("snd_pcm_hw_params_get_period_size", rc)?;
            let period = usize::try_from(period).unwrap_or(0).max(1);
            let format = AudioFormat::new(rate, layout, SampleFormat::F32Interleaved)?;
            Ok(Negotiated {
                format,
                kind,
                channels: layout.channel_count(),
                rate,
                period,
            })
        }

        /// Frames queued between the device and the application, or 0 if ALSA can't
        /// say.
        fn delay(&self) -> i64 {
            let mut frames: c_long = 0;
            // SAFETY: a live handle and a valid out pointer.
            let rc = unsafe { (self.api.delay)(self.handle, &raw mut frames) };
            if rc < 0 { 0 } else { frames.max(0) }
        }

        /// Waits up to [`WAIT_MS`] for the PCM: `Ok(true)` when it is ready, `Ok(false)`
        /// on a timeout, the negative code on an error.
        fn wait(&self) -> Result<bool, c_int> {
            // SAFETY: a live handle; the call blocks for at most `WAIT_MS`.
            let rc = unsafe { (self.api.wait)(self.handle, WAIT_MS) };
            if rc < 0 { Err(rc) } else { Ok(rc > 0) }
        }

        /// Recovers from an overrun, underrun or suspend: `true` when the stream can go
        /// on.
        fn recover(&self, code: c_int) -> bool {
            // SAFETY: a live handle; `silent` 1 keeps alsa-lib from printing.
            unsafe { (self.api.recover)(self.handle, code, 1) >= 0 }
        }

        /// Recovers a capture PCM and starts it again: `snd_pcm_recover` only
        /// prepares the PCM after an overrun, and a prepared capture stream records
        /// nothing until it is started, so the capture loop would wait forever (#572
        /// review B1). `true` when the stream is running again.
        fn recover_capture(&self, code: c_int) -> bool {
            if !self.recover(code) {
                return false;
            }
            if self.state() == STATE_RUNNING {
                return true;
            }
            // SAFETY: a live handle, prepared by the recovery.
            unsafe { (self.api.start)(self.handle) >= 0 }
        }

        /// The PCM's `snd_pcm_state_t`.
        fn state(&self) -> c_int {
            // SAFETY: a live handle.
            unsafe { (self.api.state)(self.handle) }
        }

        fn lost(&self, code: c_int) -> PlatformError {
            PlatformError::DeviceLost {
                what: format!("ALSA device {:?}", self.id),
                reason: self.api.message(code),
            }
        }
    }

    /// Audio input and output through ALSA (ADR-0060 §4): the fallback where no
    /// PipeWire daemon runs. It records from and plays to ALSA's PCMs; desktop and
    /// application audio are `Unsupported`. Each stream runs on a thread of its own.
    #[derive(Debug)]
    pub struct AlsaAudio {
        api: Arc<Api>,
        clock: Arc<MonotonicClock>,
    }

    impl AlsaAudio {
        /// Loads libasound.
        ///
        /// # Errors
        /// [`PlatformError::MissingComponent`] when libasound is missing or lacks a
        /// function the backend calls, and [`PlatformError::Os`] if the media clock
        /// can't be read.
        pub fn new() -> Result<Self, PlatformError> {
            Ok(Self {
                api: Arc::new(Api::load()?),
                clock: Arc::new(MonotonicClock::new()?),
            })
        }

        fn input_id(target: &AudioInputTarget) -> Result<&str, PlatformError> {
            match target {
                AudioInputTarget::DefaultInput => Ok("default"),
                AudioInputTarget::Input(id) => Ok(id.as_str()),
                AudioInputTarget::DefaultOutputLoopback | AudioInputTarget::OutputLoopback(_) => {
                    Err(PlatformError::Unsupported {
                        what:
                            "desktop audio through ALSA, which has no loopback (it needs PipeWire)"
                                .into(),
                    })
                }
                AudioInputTarget::ProcessLoopback { .. } => Err(PlatformError::Unsupported {
                    what: "application audio through ALSA (it needs PipeWire)".into(),
                }),
                _ => Err(PlatformError::Unsupported {
                    what: format!("audio input {target:?} through ALSA"),
                }),
            }
        }

        fn output_id(target: &AudioOutputTarget) -> Result<&str, PlatformError> {
            match target {
                AudioOutputTarget::DefaultOutput => Ok("default"),
                AudioOutputTarget::Output(id) => Ok(id.as_str()),
                _ => Err(PlatformError::Unsupported {
                    what: format!("audio output {target:?} through ALSA"),
                }),
            }
        }

        /// One hint's value for `key`, freed after copying.
        fn hint(&self, hint: *const c_void, key: &CStr) -> Option<String> {
            // SAFETY: `hint` is an element of a live hint array and `key` NUL-terminated.
            let ptr = unsafe { (self.api.name_get_hint)(hint, key.as_ptr()) };
            if ptr.is_null() {
                return None;
            }
            // SAFETY: non-null and NUL-terminated, allocated with malloc by alsa-lib.
            let text = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: alsa-lib's documentation says the caller frees it with free().
            unsafe { libc::free(ptr.cast()) };
            Some(text)
        }
    }

    impl AudioIo for AlsaAudio {
        fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError> {
            let mut hints: *mut *mut c_void = std::ptr::null_mut();
            // SAFETY: card -1 is every card; "pcm" is NUL-terminated; `hints` is a valid
            // out pointer.
            let rc = unsafe { (self.api.name_hint)(-1, c"pcm".as_ptr(), &raw mut hints) };
            if rc < 0 || hints.is_null() {
                return Err(self.api.error("snd_device_name_hint", rc, "pcm"));
            }
            let mut devices = Vec::new();
            let mut at = 0;
            loop {
                // SAFETY: the array is null-terminated, and `at` never passes the null.
                let hint = unsafe { *hints.add(at) };
                if hint.is_null() {
                    break;
                }
                at += 1;
                let Some(id) = self.hint(hint, c"NAME") else {
                    continue;
                };
                if !listed(&id) {
                    continue;
                }
                let name = device_name(self.hint(hint, c"DESC").as_deref(), &id);
                let directions: &[AudioDirection] = match self.hint(hint, c"IOID").as_deref() {
                    Some("Input") => &[AudioDirection::Input],
                    Some("Output") => &[AudioDirection::Output],
                    _ => &[AudioDirection::Input, AudioDirection::Output],
                };
                for direction in directions {
                    devices.push(AudioDeviceInfo {
                        id: DeviceId::new(id.clone()),
                        name: name.clone(),
                        direction: *direction,
                        is_default: id == "default",
                    });
                }
            }
            // SAFETY: `hints` came from `snd_device_name_hint` and is freed once.
            unsafe { (self.api.name_free_hint)(hints) };
            Ok(devices)
        }

        fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError> {
            Err(PlatformError::Unsupported {
                what: "listing applications playing audio through ALSA (it needs PipeWire)".into(),
            })
        }

        fn input_format(&self, target: &AudioInputTarget) -> Result<AudioFormat, PlatformError> {
            let id = Self::input_id(target)?;
            let pcm = Pcm::open(&self.api, id, STREAM_CAPTURE)?;
            Ok(pcm.configure()?.format)
        }

        fn open_input(
            &self,
            target: &AudioInputTarget,
            handler: Box<dyn AudioInputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            let id = Self::input_id(target)?;
            let pcm = Pcm::open(&self.api, id, STREAM_CAPTURE)?;
            let negotiated = pcm.configure()?;
            // SAFETY: a live, configured handle.
            pcm.check("snd_pcm_start", unsafe { (self.api.start)(pcm.handle) })?;
            let clock = Arc::clone(&self.clock);
            AlsaStream::spawn("jstream-alsa-in", negotiated.format, move |stop| {
                capture(&pcm, negotiated, &clock, handler, stop);
            })
        }

        fn open_output(
            &self,
            target: &AudioOutputTarget,
            handler: Box<dyn AudioOutputHandler>,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            let id = Self::output_id(target)?;
            let pcm = Pcm::open(&self.api, id, STREAM_PLAYBACK)?;
            let negotiated = pcm.configure()?;
            let clock = Arc::clone(&self.clock);
            AlsaStream::spawn("jstream-alsa-out", negotiated.format, move |stop| {
                playback(&pcm, negotiated, &clock, handler, stop);
            })
        }
    }

    /// A running stream: its thread, stopped and joined on drop.
    struct AlsaStream {
        format: AudioFormat,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl AlsaStream {
        fn spawn(
            name: &str,
            format: AudioFormat,
            run: impl FnOnce(&AtomicBool) + Send + 'static,
        ) -> Result<Box<dyn AudioStream>, PlatformError> {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let thread = std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || run(&flag))
                .map_err(|e| PlatformError::Os {
                    call: "std::thread::spawn",
                    code: e.raw_os_error().and_then(|c| u32::try_from(c).ok()),
                    message: e.to_string(),
                })?;
            Ok(Box::new(AlsaStream {
                format,
                stop,
                thread: Some(thread),
            }))
        }
    }

    impl AudioStream for AlsaStream {
        fn format(&self) -> AudioFormat {
            self.format
        }
    }

    impl Drop for AlsaStream {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                // The thread wakes within `WAIT_MS`; a panic in the handler has already
                // ended it.
                let _ = thread.join();
            }
        }
    }

    fn time_at(clock: &MonotonicClock, offset: i64) -> MediaTime {
        let now = clock.now();
        MediaTime::new(now.ticks().saturating_add(offset), now.timebase())
    }

    /// The capture thread: reads whatever is ready, a period at a time, until `stop`.
    /// Buffers are allocated before the loop, so the loop itself never allocates.
    fn capture(
        pcm: &Pcm,
        n: Negotiated,
        clock: &MonotonicClock,
        mut handler: Box<dyn AudioInputHandler>,
        stop: &AtomicBool,
    ) {
        let mut raw = vec![0_u8; n.period * n.channels * n.kind.bytes()];
        let mut samples = vec![0_f32; n.period * n.channels];
        let mut position: u64 = 0;
        let mut discontinuity = false;
        let period = c_ulong::try_from(n.period).unwrap_or(c_ulong::MAX);
        while !stop.load(Ordering::Acquire) {
            match pcm.wait() {
                Ok(false) => continue,
                Ok(true) => {}
                Err(code) => {
                    if pcm.recover_capture(code) {
                        discontinuity = true;
                        continue;
                    }
                    handler.on_error(&pcm.lost(code));
                    return;
                }
            }
            // SAFETY: `raw` holds `period` frames of `n.channels` samples of the
            // configured kind, and the handle is live.
            let read = unsafe { (pcm.api.readi)(pcm.handle, raw.as_mut_ptr().cast(), period) };
            if read == EAGAIN || read == 0 {
                continue;
            }
            if read < 0 {
                let code = c_int::try_from(read).unwrap_or(c_int::MIN);
                if pcm.recover_capture(code) {
                    discontinuity = true;
                    continue;
                }
                handler.on_error(&pcm.lost(code));
                return;
            }
            let frames = usize::try_from(read).unwrap_or(0).min(n.period);
            let count = frames * n.channels;
            to_f32(
                n.kind,
                &raw[..count * n.kind.bytes()],
                &mut samples[..count],
            );
            let queued = pcm.delay();
            let frames_i = i64::try_from(frames).unwrap_or(i64::MAX);
            let time = time_at(
                clock,
                -hns_for_frames(queued.saturating_add(frames_i), n.rate),
            );
            handler.on_packet(&AudioPacket {
                samples: &samples[..count],
                device_position: position,
                time: Some(time),
                flags: AudioPacketFlags {
                    discontinuity,
                    silent: false,
                },
            });
            discontinuity = false;
            position += frames as u64;
        }
    }

    /// The playback thread: whenever a period fits, asks the handler for it and writes
    /// it, until `stop`. Buffers are allocated before the loop.
    fn playback(
        pcm: &Pcm,
        n: Negotiated,
        clock: &MonotonicClock,
        mut handler: Box<dyn AudioOutputHandler>,
        stop: &AtomicBool,
    ) {
        let mut raw = vec![0_u8; n.period * n.channels * n.kind.bytes()];
        let mut samples = vec![0_f32; n.period * n.channels];
        let mut position: u64 = 0;
        let period = c_long::try_from(n.period).unwrap_or(c_long::MAX);
        while !stop.load(Ordering::Acquire) {
            // SAFETY: a live handle.
            let avail = unsafe { (pcm.api.avail_update)(pcm.handle) };
            if avail < 0 {
                let code = c_int::try_from(avail).unwrap_or(c_int::MIN);
                if pcm.recover(code) {
                    continue;
                }
                handler.on_error(&pcm.lost(code));
                return;
            }
            if avail < period {
                if let Err(code) = pcm.wait()
                    && !pcm.recover(code)
                {
                    handler.on_error(&pcm.lost(code));
                    return;
                }
                continue;
            }
            let time = time_at(clock, hns_for_frames(pcm.delay(), n.rate));
            samples.fill(0.0);
            handler.render(&mut AudioRenderBuffer {
                samples: &mut samples,
                device_position: position,
                time: Some(time),
            });
            from_f32(n.kind, &samples, &mut raw);
            // Write the whole period, so a short write doesn't drop its end (#572
            // review N2). An underrun abandons the rest of it after recovering.
            let frame_bytes = n.channels * n.kind.bytes();
            let mut done = 0_usize;
            while done < n.period && !stop.load(Ordering::Acquire) {
                let rest = &raw[done * frame_bytes..];
                let frames = c_ulong::try_from(n.period - done).unwrap_or(c_ulong::MAX);
                // SAFETY: `rest` holds `n.period - done` frames of the configured kind,
                // and the handle is live.
                let written = unsafe { (pcm.api.writei)(pcm.handle, rest.as_ptr().cast(), frames) };
                if written == EAGAIN {
                    if let Err(code) = pcm.wait() {
                        if pcm.recover(code) {
                            break;
                        }
                        handler.on_error(&pcm.lost(code));
                        return;
                    }
                    continue;
                }
                if written < 0 {
                    let code = c_int::try_from(written).unwrap_or(c_int::MIN);
                    if pcm.recover(code) {
                        break;
                    }
                    handler.on_error(&pcm.lost(code));
                    return;
                }
                done += usize::try_from(written).unwrap_or(0);
            }
            position += done as u64;
        }
    }

    /// Recovery on ALSA's `null` PCM, which needs no sound card; skipped where
    /// libasound isn't installed.
    #[cfg(test)]
    mod recovery_tests {
        use std::sync::Arc;

        use super::{Api, Pcm, STATE_RUNNING, STREAM_CAPTURE};

        /// `-EPIPE`, an overrun.
        const EPIPE: i32 = -32;
        /// `SND_PCM_STATE_PREPARED`.
        const STATE_PREPARED: i32 = 2;

        fn running_capture() -> Option<Pcm> {
            let api = Arc::new(Api::load().ok()?);
            let pcm = Pcm::open(&api, "null", STREAM_CAPTURE).expect("open null");
            pcm.configure().expect("configure null");
            // SAFETY: a live, configured handle.
            let rc = unsafe { (pcm.api.start)(pcm.handle) };
            assert!(rc >= 0, "start: {rc}");
            assert_eq!(pcm.state(), STATE_RUNNING);
            Some(pcm)
        }

        /// The regression for review B1: recovering alone leaves a capture PCM
        /// prepared, which records nothing; `recover_capture` has it running again.
        #[test]
        fn an_overrun_capture_is_running_again_after_recovery() {
            let Some(pcm) = running_capture() else { return };
            assert!(pcm.recover(EPIPE));
            assert_eq!(pcm.state(), STATE_PREPARED, "recover only prepares");

            let Some(pcm) = running_capture() else { return };
            assert!(pcm.recover_capture(EPIPE));
            assert_eq!(pcm.state(), STATE_RUNNING);
            assert_eq!(pcm.wait(), Ok(true), "data flows again");
        }
    }
}

#[cfg(test)]
mod tests;
