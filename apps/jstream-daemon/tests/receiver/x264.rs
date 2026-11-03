//! A test stand-in for Linux's software H.264 encoder: the system's `ffmpeg` command
//! encoding with libx264, behind the same `VideoEncoder` trait OpenH264 implements.
//!
//! It exists because Cisco's OpenH264, the product's software encoder, can't be loaded
//! on a machine that hasn't downloaded it (ADR-0061), and the harness has to run in a
//! sandbox with no route to Cisco. Nothing in JStream encodes through FFmpeg
//! (R-DEP-002); this file is test code that drives the `ffmpeg` program in a child
//! process, the way the harness drives `ffprobe`, and is never linked into a product.
//! When `JSTREAM_E2E_OPENH264` is set (CI's run downloads Cisco's library) the harness
//! uses OpenH264 instead.
//!
//! Frames are written raw to the child's stdin from a writer thread; a reader thread
//! splits its Annex B output into access units at each access unit delimiter (x264's
//! `aud=1`), which it then strips, so packets look like OpenH264's: one access unit
//! each, SPS and PPS in front of every IDR (`repeat-headers=1`). B-frames are off, so
//! access units come out in input order and take their frames' timestamps in turn.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};

use jstream_media::{Codec, MediaTime, Packet, PixelFormat};
use jstream_platform::{
    CodecInfo, CodecRole, HardwarePreference, PlatformError, RateControl, VideoEncoder,
    VideoEncoderConfig, VideoSurface,
};

/// The name the stand-in lists under.
pub const NAME: &str = "ffmpeg libx264 (test stand-in)";

/// Frames in flight (queued for the writer, in the pipe or in ffmpeg) before
/// `can_accept` says no; also the writer's queue.
const QUEUE: usize = 16;

pub fn info() -> CodecInfo {
    CodecInfo {
        name: NAME.into(),
        codec: Codec::H264,
        role: CodecRole::VideoEncoder,
        hardware: false,
    }
}

fn os(what: impl Into<String>) -> PlatformError {
    PlatformError::Os {
        call: "ffmpeg",
        code: None,
        message: what.into(),
    }
}

/// One libx264 encode in an `ffmpeg` child.
pub struct X264 {
    info: CodecInfo,
    config: VideoEncoderConfig,
    child: Child,
    frames: Option<SyncSender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
    units: Receiver<(Vec<u8>, bool)>,
    reader: Option<JoinHandle<()>>,
    /// The `pts` of each frame sent whose access unit hasn't come back yet.
    pending: VecDeque<MediaTime>,
}

impl X264 {
    /// Starts `ffmpeg` for `config`.
    pub fn open(config: &VideoEncoderConfig) -> Result<Self, PlatformError> {
        if config.codec != Codec::H264 || config.hardware == HardwarePreference::Require {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "{:?} ({:?}) from the libx264 stand-in",
                    config.codec, config.hardware
                ),
            });
        }
        let pix_fmt = match config.input_format {
            PixelFormat::Nv12 => "nv12",
            PixelFormat::I420 => "yuv420p",
            PixelFormat::Bgra8 => "bgra",
            PixelFormat::Rgba8 => "rgba",
            other => {
                return Err(PlatformError::Unsupported {
                    what: format!("{other:?} input to the libx264 stand-in"),
                });
            }
        };
        let (kbps, max_kbps) = match config.rate_control {
            RateControl::Cbr { kbps } => (kbps, kbps),
            RateControl::Vbr { kbps, max_kbps } => (kbps, max_kbps),
            other => {
                return Err(PlatformError::Unsupported {
                    what: format!("{other:?} on the libx264 stand-in"),
                });
            }
        };
        let period = config.frame_period;
        // The frame rate is the period's inverse.
        let rate = format!("{}/{}", period.den(), period.num());
        let size = format!("{}x{}", config.size.width(), config.size.height());
        let keyint = config.keyframe_interval.max(1).to_string();
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
            .args([
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix_fmt,
                "-s",
                &size,
                "-framerate",
                &rate,
            ])
            .args(["-i", "pipe:0", "-an", "-c:v", "libx264"])
            .args(["-preset", "ultrafast", "-tune", "zerolatency", "-bf", "0"])
            .args(["-g", &keyint, "-keyint_min", &keyint, "-sc_threshold", "0"])
            .args([
                "-b:v",
                &format!("{kbps}k"),
                "-maxrate",
                &format!("{max_kbps}k"),
            ])
            .args(["-bufsize", &format!("{max_kbps}k")])
            .args(["-x264-params", "aud=1:repeat-headers=1"])
            .args(["-color_range", "tv", "-colorspace", "bt709"])
            .args(["-color_primaries", "bt709", "-color_trc", "bt709"])
            .args(["-f", "h264", "pipe:1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| os(format!("starting ffmpeg: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (frames, queued) = sync_channel::<Vec<u8>>(QUEUE);
        let writer = thread::spawn(move || write_frames(stdin, &queued));
        let (send, units) = sync_channel(1024);
        let reader = thread::spawn(move || read_units(stdout, &send));
        Ok(Self {
            info: info(),
            config: *config,
            child,
            frames: Some(frames),
            writer: Some(writer),
            units,
            reader: Some(reader),
            pending: VecDeque::new(),
        })
    }

    /// Turns every access unit that has come back into a packet.
    fn drain(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        loop {
            let (data, keyframe) = match self.units.try_recv() {
                Ok(unit) => unit,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(()),
            };
            let pts = self
                .pending
                .pop_front()
                .ok_or_else(|| os("ffmpeg sent more access units than frames"))?;
            out.push(
                Packet::new(Codec::H264, keyframe, pts, pts, data)
                    .map_err(|e| os(format!("packet: {e}")))?,
            );
        }
    }
}

/// The frame's planes, rows without padding, in the order rawvideo reads them.
fn raw(frame: &VideoSurface) -> Result<Vec<u8>, PlatformError> {
    let VideoSurface::Cpu(frame) = frame else {
        return Err(PlatformError::Unsupported {
            what: "a GPU frame on the libx264 stand-in".into(),
        });
    };
    let mut bytes = Vec::new();
    for plane in 0..frame.planes().len() {
        let mut y = 0;
        while let Some(row) = frame.row(plane, y) {
            bytes.extend_from_slice(row);
            y += 1;
        }
    }
    Ok(bytes)
}

fn write_frames(mut stdin: ChildStdin, frames: &Receiver<Vec<u8>>) {
    while let Ok(frame) = frames.recv() {
        if stdin.write_all(&frame).is_err() {
            return;
        }
    }
    // Dropping stdin ends ffmpeg's input, so it drains.
}

/// Where each NAL unit of an Annex B stream starts, its start code included.
fn nal_starts(stream: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            // Include a leading zero of a four-byte start code.
            let at = if i > 0 && stream[i - 1] == 0 {
                i - 1
            } else {
                i
            };
            starts.push(at);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
}

fn nal_type(stream: &[u8], start: usize) -> u8 {
    let header = if stream[start + 2] == 1 {
        start + 3
    } else {
        start + 4
    };
    stream.get(header).map_or(0, |b| b & 0x1f)
}

/// Sends each complete access unit, without its delimiter, and whether it holds an IDR
/// slice. A unit is complete when the next delimiter arrives, or at the end.
fn read_units(mut stdout: impl Read, send: &SyncSender<(Vec<u8>, bool)>) {
    let mut stream = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    let mut ended = false;
    while !ended {
        match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => ended = true,
            Ok(n) => stream.extend_from_slice(&chunk[..n]),
        }
        let starts = nal_starts(&stream);
        let delimiters: Vec<usize> = starts
            .iter()
            .copied()
            .filter(|&s| nal_type(&stream, s) == 9)
            .collect();
        // Every unit between two delimiters is complete; at the end, the last one too.
        let mut complete: Vec<(usize, usize)> =
            delimiters.windows(2).map(|w| (w[0], w[1])).collect();
        if ended && let Some(&last) = delimiters.last() {
            complete.push((last, stream.len()));
        }
        let consumed = complete.last().map_or(0, |&(_, end)| end);
        for (from, to) in complete {
            let mut unit = Vec::new();
            let mut keyframe = false;
            let inner: Vec<usize> = starts
                .iter()
                .copied()
                .filter(|&s| s >= from && s < to)
                .collect();
            for (k, &s) in inner.iter().enumerate() {
                let end = inner.get(k + 1).copied().unwrap_or(to);
                match nal_type(&stream, s) {
                    9 => {}
                    t => {
                        keyframe |= t == 5;
                        unit.extend_from_slice(&stream[s..end]);
                    }
                }
            }
            if !unit.is_empty() && send.send((unit, keyframe)).is_err() {
                return;
            }
        }
        stream.drain(..consumed);
    }
}

impl VideoEncoder for X264 {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        // Frames sent whose access unit hasn't come back: the writer's queue, the pipe
        // and ffmpeg's own delay. `encode` refuses only past the writer's queue, so
        // saying no here first holds frames back instead of dropping them.
        self.frames.is_some() && self.pending.len() < QUEUE
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        let Some(frames) = &self.frames else {
            return Err(PlatformError::InvalidState {
                what: "encode",
                state: "finished",
            });
        };
        let info = frame.info();
        if info.size != self.config.size || info.format != self.config.input_format {
            return Err(PlatformError::Unsupported {
                what: format!("a {:?} {:?} frame", info.size, info.format),
            });
        }
        let pts = info.pts;
        match frames.try_send(raw(&frame)?) {
            Ok(()) => {
                self.pending.push_back(pts);
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(PlatformError::Busy {
                what: "the libx264 stand-in's input queue",
            }),
            Err(TrySendError::Disconnected(_)) => Err(os("ffmpeg's input closed")),
        }
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.drain(out)
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.frames = None;
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.drain(out)?;
        let _ = self.child.wait();
        self.pending.clear();
        Ok(())
    }
}

impl Drop for X264 {
    fn drop(&mut self) {
        self.frames = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
