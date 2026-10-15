//! Audio device I/O: capture endpoints, loopback, and output devices.
//!
//! Each open stream runs on a backend-owned real-time audio thread (one per stream,
//! docs/ARCHITECTURE.md#threading) and calls an engine-supplied handler for every device
//! packet. The handler does the engine's per-device work there (drift tracking and
//! adaptive resampling into the engine's ring, per docs/adr/0014-media-clock.md), so it
//! must not allocate, lock, block or log (R-RT-001).

use jstream_media::{AudioFormat, MediaTime};

use crate::{DeviceId, PlatformError};

/// Whether a device records or plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioDirection {
    /// A capture endpoint, such as a microphone.
    Input,
    /// A render endpoint, such as speakers. Its loopback can also be captured.
    Output,
}

/// An audio device as [`AudioIo::devices`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    /// The device's ID.
    pub id: DeviceId,
    /// A human-readable name.
    pub name: String,
    /// Whether it records or plays.
    pub direction: AudioDirection,
    /// Whether it is the system's default device for its direction.
    pub is_default: bool,
}

/// A running process, as [`AudioIo::processes`] lists it: what process loopback
/// ([`AudioInputTarget::ProcessLoopback`]) can record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioProcess {
    /// The process ID.
    pub pid: u32,
    /// The ID of the process that started it, if the OS reports one. It may have
    /// exited, and its ID may have been reused since.
    pub parent_pid: Option<u32>,
    /// The executable's file name, such as `obs64.exe`, without its folder.
    pub exe: String,
}

/// What an input stream records.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioInputTarget {
    /// The default capture device. It follows the system default when that changes.
    DefaultInput,
    /// One capture device.
    Input(DeviceId),
    /// Everything the default output device plays (desktop audio).
    DefaultOutputLoopback,
    /// Everything one output device plays.
    OutputLoopback(DeviceId),
    /// The audio of one process and its children (process loopback on Windows).
    ProcessLoopback {
        /// The process ID.
        pid: u32,
    },
}

/// Where an output stream plays.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioOutputTarget {
    /// The default output device. It follows the system default when that changes.
    DefaultOutput,
    /// One output device.
    Output(DeviceId),
}

/// The audio-device backend: lists devices and opens streams on them.
///
/// Shared (`Arc<dyn AudioIo>`) and called from the control thread, so it is
/// `Send + Sync`.
pub trait AudioIo: Send + Sync {
    /// The input and output devices present now.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the OS refuses to enumerate them.
    fn devices(&self) -> Result<Vec<AudioDeviceInfo>, PlatformError>;

    /// The processes running now, any of which process loopback can record.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the OS refuses to list them.
    fn processes(&self) -> Result<Vec<AudioProcess>, PlatformError>;

    /// The format [`open_input`](AudioIo::open_input) would deliver `target` in if it
    /// were opened now, so the engine can prepare a handler for it. The device can
    /// still change before the open: the opened stream's
    /// [`format`](AudioStream::format) is the one that counts.
    ///
    /// # Errors
    /// As for [`open_input`](AudioIo::open_input).
    fn input_format(&self, target: &AudioInputTarget) -> Result<AudioFormat, PlatformError>;

    /// Starts recording `target`, calling `handler` on the backend's audio thread for
    /// every packet. Dropping the returned stream stops it, and the handler is dropped
    /// once its last call has returned.
    ///
    /// The backend picks the device's own format (its shared-mode mix format) and
    /// converts samples to interleaved `f32`; it never resamples. That is the engine's
    /// job (ADR-0014).
    ///
    /// # Errors
    /// [`PlatformError::NotFound`] when the device is gone,
    /// [`PlatformError::Unsupported`] for a target kind or device format the backend
    /// can't serve, and [`PlatformError::Os`] when the OS refuses.
    fn open_input(
        &self,
        target: &AudioInputTarget,
        handler: Box<dyn AudioInputHandler>,
    ) -> Result<Box<dyn AudioStream>, PlatformError>;

    /// Starts playing on `target`, calling `handler` on the backend's audio thread
    /// whenever the device needs more samples. Dropping the returned stream stops it.
    ///
    /// # Errors
    /// As for [`open_input`](AudioIo::open_input).
    fn open_output(
        &self,
        target: &AudioOutputTarget,
        handler: Box<dyn AudioOutputHandler>,
    ) -> Result<Box<dyn AudioStream>, PlatformError>;
}

/// A running input or output stream. Dropping it stops the stream.
pub trait AudioStream: Send {
    /// The format the handler's samples are in: the device's rate and channel layout,
    /// always interleaved `f32`.
    fn format(&self) -> AudioFormat;
}

/// Engine code called on the audio thread for each packet an input stream records.
///
/// Both methods run on the real-time audio thread: no allocating, locking, blocking or
/// logging (R-RT-001).
pub trait AudioInputHandler: Send {
    /// One device packet.
    fn on_packet(&mut self, packet: &AudioPacket<'_>);

    /// The stream failed and delivers nothing more, for example with
    /// [`PlatformError::DeviceLost`]. Called at most once.
    fn on_error(&mut self, error: &PlatformError);
}

/// One packet from an input device, as the device reported it.
///
/// It carries the device's own position and timestamp rather than a corrected time, so
/// the engine can estimate the device's drift against the media clock (spike S3 fits
/// device position against QPC time). The backend only maps the timestamp onto the media
/// clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioPacket<'a> {
    /// The samples, interleaved `f32`, a whole number of frames in the stream's
    /// [`format`](AudioStream::format). Undefined when `flags.silent` is set.
    pub samples: &'a [f32],
    /// The device position of the first frame: frames since the stream started,
    /// including frames lost in a glitch. A jump forward means frames were lost.
    pub device_position: u64,
    /// When the first frame was recorded, on the [`MediaClock`](crate::MediaClock).
    /// `None` when the device flagged the timestamp as unreliable.
    pub time: Option<MediaTime>,
    /// The device's flags for this packet.
    pub flags: AudioPacketFlags,
}

/// Per-packet flags an input device reports (WASAPI's `AUDCLNT_BUFFERFLAGS_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct AudioPacketFlags {
    /// The data is not continuous with the previous packet.
    pub discontinuity: bool,
    /// Treat the packet as silence; `samples` is undefined.
    pub silent: bool,
}

/// Engine code called on the audio thread whenever an output device needs samples.
///
/// It runs on the real-time audio thread: no allocating, locking, blocking or logging
/// (R-RT-001).
pub trait AudioOutputHandler: Send {
    /// Fills `buffer.samples` completely (with silence if there is nothing to play).
    fn render(&mut self, buffer: &mut AudioRenderBuffer<'_>);

    /// The stream failed and asks for nothing more. Called at most once.
    fn on_error(&mut self, error: &PlatformError);
}

/// The space an output handler fills.
#[derive(Debug, PartialEq)]
pub struct AudioRenderBuffer<'a> {
    /// The samples to write, interleaved `f32`, a whole number of frames in the stream's
    /// [`format`](AudioStream::format).
    pub samples: &'a mut [f32],
    /// The device position of the first frame: frames since the stream started.
    pub device_position: u64,
    /// When the first frame is expected to play, on the
    /// [`MediaClock`](crate::MediaClock), if the device reports it.
    pub time: Option<MediaTime>,
}
