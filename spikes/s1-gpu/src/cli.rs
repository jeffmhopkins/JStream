//! Command-line options.

use std::path::PathBuf;

pub const USAGE: &str = "\
s1-gpu: WGC -> wgpu (D3D12) -> NV12 -> Media Foundation H.264, no CPU copy

USAGE:
    cargo run --release -- [OPTIONS]

OPTIONS:
    --seconds <N>      How long to capture and encode [default: 30]
    --size <WxH>       Encode size, or `native` for the display size [default: 1920x1080]
    --fps <N>          Target frame rate of the pipeline tick [default: 60]
    --bitrate <KBPS>   H.264 CBR bitrate in kbit/s [default: 12000]
    --out <PATH>       Elementary H.264 (Annex B) output [default: out.h264]
    --json             Print the result as one JSON object
    -h, --help         Print this help";

/// Requested encode size.
#[derive(Clone, Copy, Debug)]
pub enum Size {
    /// The captured display's size (rounded down to even numbers for NV12).
    Native,
    /// A fixed size; the converter scales with bilinear filtering.
    Fixed(u32, u32),
}

#[derive(Debug)]
pub struct Args {
    pub seconds: f64,
    pub size: Size,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub out: PathBuf,
    pub json: bool,
}

impl Args {
    /// Parses the arguments. `Ok(None)` means help was requested.
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = Args {
            seconds: 30.0,
            size: Size::Fixed(1920, 1080),
            fps: 60,
            bitrate_kbps: 12_000,
            out: PathBuf::from("out.h264"),
            json: false,
        };
        while let Some(flag) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match flag.as_str() {
                "-h" | "--help" => return Ok(None),
                "--json" => args.json = true,
                "--seconds" => {
                    args.seconds = parse_num(&value("--seconds")?)?;
                    if args.seconds.is_nan() || args.seconds <= 0.0 {
                        return Err("--seconds must be > 0".into());
                    }
                }
                "--size" => args.size = parse_size(&value("--size")?)?,
                "--fps" => args.fps = parse_num::<u32>(&value("--fps")?)?.max(1),
                "--bitrate" => args.bitrate_kbps = parse_num::<u32>(&value("--bitrate")?)?.max(100),
                "--out" => args.out = PathBuf::from(value("--out")?),
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(Some(args))
    }
}

fn parse_num<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("`{s}` is not a valid number"))
}

fn parse_size(s: &str) -> Result<Size, String> {
    if s.eq_ignore_ascii_case("native") {
        return Ok(Size::Native);
    }
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("`{s}` is not WxH or `native`"))?;
    let (w, h): (u32, u32) = (parse_num(w)?, parse_num(h)?);
    if w < 64 || h < 64 || w % 2 != 0 || h % 2 != 0 {
        return Err(format!(
            "`{s}`: width and height must be even and at least 64"
        ));
    }
    Ok(Size::Fixed(w, h))
}
