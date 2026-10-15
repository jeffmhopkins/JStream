//! Command-line options.

pub const USAGE: &str = "\
s4-browser: CEF off-screen rendering -> shared D3D texture -> wgpu (D3D12), plus CEF's
audio handler. Spike S4 (ADR-0021).

USAGE:
    cargo run --release -- [OPTIONS]

OPTIONS:
    --seconds <N>        Length of the measured window [default: 30]
    --warmup <N>         Seconds after the browser starts before measuring [default: 3]
    --size <WxH>         Page (view) size in pixels [default: 1920x1080]
    --fps <N>            CEF windowless frame rate, 1..=60 [default: 60]
    --helper <MODE>      Where CEF's subprocesses run: `same` (this exe again, with a
                         subprocess flag) or `separate` (s4-browser-helper.exe)
                         [default: same]
    --url <URL>          Load this page instead of the built-in test page. The frame
                         counter, pixel probes and tone checks then report null
    --cef-log <PATH>     CEF's own log file [default: s4-cef.log]
    --json               Print the result as one JSON object
    -h, --help           Print this help";

/// Where CEF's renderer, GPU and utility subprocesses run (ADR-0021, Packaging).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Helper {
    /// The main exe again: `execute_process` runs first thing in `main` and returns the
    /// subprocess's exit code when CEF passed `--type=...`.
    Same,
    /// `s4-browser-helper.exe` next to the main exe, through `browser_subprocess_path`.
    Separate,
}

impl Helper {
    pub fn name(self) -> &'static str {
        match self {
            Helper::Same => "same",
            Helper::Separate => "separate",
        }
    }
}

#[derive(Debug)]
pub struct Args {
    pub seconds: f64,
    pub warmup: f64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub helper: Helper,
    pub url: Option<String>,
    pub cef_log: String,
    pub json: bool,
}

impl Args {
    /// Parses the arguments. `Ok(None)` means help was requested.
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = Args {
            seconds: 30.0,
            warmup: 3.0,
            width: 1920,
            height: 1080,
            fps: 60,
            helper: Helper::Same,
            url: None,
            cef_log: "s4-cef.log".into(),
            json: false,
        };
        while let Some(flag) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match flag.as_str() {
                "-h" | "--help" => return Ok(None),
                "--json" => args.json = true,
                "--seconds" => args.seconds = parse_secs("--seconds", &value("--seconds")?)?,
                "--warmup" => args.warmup = parse_secs("--warmup", &value("--warmup")?)?,
                "--size" => (args.width, args.height) = parse_size(&value("--size")?)?,
                "--fps" => {
                    args.fps = parse_num(&value("--fps")?)?;
                    // CEF clamps windowless_frame_rate to 1..=60.
                    if !(1..=60).contains(&args.fps) {
                        return Err("--fps must be 1..=60".into());
                    }
                }
                "--helper" => {
                    args.helper = match value("--helper")?.as_str() {
                        "same" => Helper::Same,
                        "separate" => Helper::Separate,
                        other => return Err(format!("--helper `{other}`: use same or separate")),
                    }
                }
                "--url" => args.url = Some(value("--url")?),
                "--cef-log" => args.cef_log = value("--cef-log")?,
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

fn parse_secs(name: &str, s: &str) -> Result<f64, String> {
    let v: f64 = parse_num(s)?;
    if !v.is_finite() || v < 0.0 || (name == "--seconds" && v == 0.0) {
        return Err(format!(
            "{name} must be a finite number >= 0 (> 0 for --seconds)"
        ));
    }
    Ok(v)
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("`{s}` is not WxH"))?;
    let (w, h): (u32, u32) = (parse_num(w)?, parse_num(h)?);
    // The test page's probe row needs 640 x 80 pixels.
    if w < 640 || h < 80 || w > 7680 || h > 4320 {
        return Err(format!(
            "`{s}`: width must be 640..=7680 and height 80..=4320"
        ));
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>, String> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn defaults() {
        let a = parse(&[]).expect("parses").expect("not help");
        assert_eq!((a.width, a.height, a.fps), (1920, 1080, 60));
        assert_eq!(a.helper, Helper::Same);
        assert!(a.url.is_none());
    }

    #[test]
    fn options() {
        let a = parse(&[
            "--size", "1280x720", "--fps", "30", "--helper", "separate", "--json",
        ])
        .expect("parses")
        .expect("not help");
        assert_eq!((a.width, a.height, a.fps), (1280, 720, 30));
        assert_eq!(a.helper, Helper::Separate);
        assert!(a.json);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(parse(&["--fps", "120"]).is_err());
        assert!(parse(&["--size", "320x200"]).is_err());
        assert!(parse(&["--helper", "other"]).is_err());
        assert!(parse(&["--seconds", "0"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--help"]).expect("parses").is_none());
    }
}
