//! Command-line options.

use std::path::PathBuf;

pub const USAGE: &str = "\
s6-metal: IOSurface -> wgpu (Metal) -> NV12 IOSurface -> VideoToolbox H.264, no CPU copy

USAGE:
    cargo run --release -- [OPTIONS]

OPTIONS:
    --sync <MODE>      none, completion, event, event-strict, or all (each in turn)
                       [default: all]
    --seconds <N>      How long each mode runs [default: 60]
    --size <WxH>       Frame size, even, at least 256x128 [default: 1920x1080]
    --fps <N>          Target frame rate of the pipeline tick [default: 60]
    --load <N>         Producer shader work per pixel, in loop iterations, so its frames
                       take long enough on the GPU for an early read to show [default: 64]
    --bitrate <KBPS>   H.264 average bit rate in kbit/s [default: 12000]
    --out <PREFIX>     H.264 (Annex B) output, one file per mode: <PREFIX>-<mode>.h264
                       [default: out]
    --json             Print each mode's result as one JSON object
    -h, --help         Print this help

MODES:
    none          No synchronisation: the control, which should show early reads
    completion    The CPU waits for each GPU submission to complete before the next step
    event         MTLSharedEvent waits and signals staged on wgpu-hal's queue
    event-strict  As `event`, with wgpu-hal's strict event sync on the engine's queue";

/// How the producer, the engine and the consumer hand frames to each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Nothing: the control.
    None,
    /// CPU waits on each submission's completion (`Device::poll`).
    Completion,
    /// Shared events, through `add_wait_event` and `add_signal_event`.
    Event,
    /// Shared events, with `enable_strict_event_sync` on the engine's queue.
    EventStrict,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::None, Mode::Completion, Mode::Event, Mode::EventStrict];

    pub fn name(self) -> &'static str {
        match self {
            Mode::None => "none",
            Mode::Completion => "completion",
            Mode::Event => "event",
            Mode::EventStrict => "event-strict",
        }
    }

    pub fn uses_events(self) -> bool {
        matches!(self, Mode::Event | Mode::EventStrict)
    }
}

#[derive(Debug)]
pub struct Args {
    /// `None` runs every mode.
    pub sync: Option<Mode>,
    pub seconds: f64,
    pub size: (u32, u32),
    pub fps: u32,
    pub load: u32,
    pub bitrate_kbps: u32,
    pub out: PathBuf,
    pub json: bool,
}

impl Args {
    /// Parses the arguments. `Ok(None)` means help was requested.
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = Args {
            sync: None,
            seconds: 60.0,
            size: (1920, 1080),
            fps: 60,
            load: 64,
            bitrate_kbps: 12_000,
            out: PathBuf::from("out"),
            json: false,
        };
        while let Some(flag) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match flag.as_str() {
                "-h" | "--help" => return Ok(None),
                "--json" => args.json = true,
                "--sync" => {
                    let v = value("--sync")?;
                    args.sync = if v == "all" {
                        None
                    } else {
                        Some(
                            Mode::ALL
                                .into_iter()
                                .find(|m| m.name() == v)
                                .ok_or_else(|| format!("unknown --sync mode `{v}`"))?,
                        )
                    };
                }
                "--seconds" => {
                    args.seconds = parse_num(&value("--seconds")?)?;
                    if !args.seconds.is_finite() || args.seconds <= 0.0 {
                        return Err("--seconds must be a finite number > 0".into());
                    }
                }
                "--size" => args.size = parse_size(&value("--size")?)?,
                "--fps" => args.fps = parse_num::<u32>(&value("--fps")?)?.max(1),
                "--load" => args.load = parse_num(&value("--load")?)?,
                "--bitrate" => args.bitrate_kbps = parse_num::<u32>(&value("--bitrate")?)?.max(100),
                "--out" => args.out = PathBuf::from(value("--out")?),
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(Some(args))
    }

    /// The modes to run, in order.
    pub fn modes(&self) -> Vec<Mode> {
        self.sync.map_or_else(|| Mode::ALL.to_vec(), |m| vec![m])
    }

    /// The H.264 output for `mode`.
    pub fn out_for(&self, mode: Mode) -> PathBuf {
        let mut name = self.out.as_os_str().to_owned();
        name.push(format!("-{}.h264", mode.name()));
        PathBuf::from(name)
    }
}

fn parse_num<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("`{s}` is not a valid number"))
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("`{s}` is not WxH"))?;
    let (w, h): (u32, u32) = (parse_num(w)?, parse_num(h)?);
    // The probe blocks need 256x128 (see `mac::PROBE`).
    if w < 256 || h < 128 || w % 2 != 0 || h % 2 != 0 {
        return Err(format!(
            "`{s}`: width and height must be even, and at least 256x128"
        ));
    }
    Ok((w, h))
}
