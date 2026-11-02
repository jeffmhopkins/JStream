//! The far end: a real RTMP server (`ffmpeg -listen 1`) that writes what it receives
//! to an FLV file, and `ffprobe`/`ffmpeg` reading that file back.

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// A free loopback port. Another process could take it before the receiver binds it;
/// on a test machine that is rare enough.
pub fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port()
}

/// Whether something listens on loopback `port`, from `/proc/net/tcp`. Connecting to
/// find out would use up the receiver's one accept.
fn listening(port: u16) -> bool {
    let wanted = format!("0100007F:{port:04X}");
    std::fs::read_to_string("/proc/net/tcp").is_ok_and(|table| {
        table.lines().skip(1).any(|line| {
            let mut fields = line.split_whitespace().skip(1);
            fields.next() == Some(wanted.as_str()) && fields.nth(1) == Some("0A")
        })
    })
}

/// One `ffmpeg` RTMP server for one publish, copying the stream into `file`.
pub struct Receiver {
    child: Child,
    pub file: PathBuf,
}

impl Receiver {
    /// Listens on `127.0.0.1:port` for a publish to `live/<key>`, and returns once it
    /// listens.
    pub fn start(port: u16, key: &str, file: &Path) -> Self {
        let _ = std::fs::remove_file(file);
        let url = format!("rtmp://127.0.0.1:{port}/live/{key}");
        let child = Command::new("ffmpeg")
            // `fatal`: the publisher hanging up is logged as an error, and it is expected.
            .args(["-hide_banner", "-nostdin", "-loglevel", "fatal", "-y"])
            .args(["-listen", "1", "-i", &url, "-c", "copy", "-f", "flv"])
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("ffmpeg runs: install it to run this test");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !listening(port) {
            assert!(Instant::now() < deadline, "the receiver never listened");
            thread::sleep(Duration::from_millis(20));
        }
        Self {
            child,
            file: file.to_owned(),
        }
    }

    /// Waits for the receiver to finish its file after the publisher hung up.
    pub fn finish(mut self) -> PathBuf {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().expect("waits") {
                assert!(status.success(), "the receiver failed: {status}");
                return self.file.clone();
            }
            assert!(Instant::now() < deadline, "the receiver never finished");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stops the receiver, as a server that goes away does: the connection drops. It
    /// gets SIGTERM, so `ffmpeg` closes its file properly before it exits.
    pub fn kill(mut self) -> PathBuf {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
        self.file.clone()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What `ffprobe` says about a received file.
#[derive(Debug)]
pub struct Probe {
    pub video_codec: String,
    pub width: u64,
    pub height: u64,
    /// `r_frame_rate`, as a number.
    pub fps: f64,
    pub audio_codec: Option<String>,
    pub sample_rate: Option<u64>,
    pub channels: Option<u64>,
    /// The file's duration, in seconds.
    pub duration: f64,
    /// Each video packet's time, in seconds, and whether it is a keyframe.
    pub video_packets: Vec<(f64, bool)>,
}

impl Probe {
    /// The seconds between consecutive keyframes.
    pub fn keyframe_gaps(&self) -> Vec<f64> {
        let keys: Vec<f64> = self
            .video_packets
            .iter()
            .filter(|(_, key)| *key)
            .map(|(t, _)| *t)
            .collect();
        keys.windows(2).map(|w| w[1] - w[0]).collect()
    }
}

fn run(command: &mut Command) -> std::process::Output {
    let output = command.output().expect("ffprobe and ffmpeg run");
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn ratio(text: &str) -> f64 {
    match text.split_once('/') {
        Some((n, d)) => {
            let (n, d): (f64, f64) = (n.parse().unwrap_or(0.0), d.parse().unwrap_or(1.0));
            if d == 0.0 { 0.0 } else { n / d }
        }
        None => text.parse().unwrap_or(0.0),
    }
}

/// Probes `file`'s streams, duration and video packets.
pub fn probe(file: &Path) -> Probe {
    let output = run(Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(file));
    let json: Value = serde_json::from_slice(&output.stdout).expect("ffprobe's JSON");
    let streams = json["streams"].as_array().cloned().unwrap_or_default();
    let of = |kind: &str| {
        streams
            .iter()
            .find(|s| s["codec_type"] == kind)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let (video, audio) = (of("video"), of("audio"));
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let packets = run(Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .args(["packet=pts_time,flags", "-of", "csv=p=0"])
        .arg(file));
    let video_packets = String::from_utf8_lossy(&packets.stdout)
        .lines()
        .filter_map(|line| {
            let (time, flags) = line.split_once(',')?;
            Some((time.parse().ok()?, flags.starts_with('K')))
        })
        .collect();
    Probe {
        video_codec: text(&video["codec_name"]).unwrap_or_default(),
        width: video["width"].as_u64().unwrap_or(0),
        height: video["height"].as_u64().unwrap_or(0),
        fps: ratio(video["r_frame_rate"].as_str().unwrap_or("0")),
        audio_codec: text(&audio["codec_name"]),
        sample_rate: audio["sample_rate"].as_str().and_then(|s| s.parse().ok()),
        channels: audio["channels"].as_u64(),
        duration: json["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        video_packets,
    }
}

/// The decoded RGB colour at `(x, y)` (in the file's pixels) of the frame at `at`
/// seconds.
pub fn pixel(file: &Path, at: f64, x: u64, y: u64) -> [u8; 3] {
    let p = probe(file);
    let output = run(Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-ss",
            &format!("{at}"),
        ])
        .arg("-i")
        .arg(file)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ]));
    let at = usize::try_from((y * p.width + x) * 3).expect("an index");
    let rgb = output
        .stdout
        .get(at..at + 3)
        .expect("the pixel is in the frame");
    [rgb[0], rgb[1], rgb[2]]
}

/// The audio's mean volume in dBFS, from `volumedetect`.
pub fn mean_volume(file: &Path) -> f64 {
    let output = run(Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(file)
        .args(["-map", "0:a:0", "-af", "volumedetect", "-f", "null", "-"]));
    let log = String::from_utf8_lossy(&output.stderr);
    log.lines()
        .find_map(|line| {
            let rest = line.split("mean_volume:").nth(1)?;
            rest.trim().trim_end_matches("dB").trim().parse().ok()
        })
        .unwrap_or_else(|| panic!("no mean_volume in: {log}"))
}
