//! Command-line options.

use std::path::PathBuf;

pub const USAGE: &str = "\
s3-clock: WASAPI mic + loopback timestamps on the QPC media clock, drift, adaptive resampler

USAGE:
    cargo run --release -- [OPTIONS]

OPTIONS:
    --minutes <N>      How long to measure [default: 60]; e.g. 2 for a smoke test
    --progress <S>     Print a progress line every S seconds [default: 10]
    --csv <PATH>       Time series, one row per second [default: drift.csv]
    --settle <S>       Seconds before residual statistics start
                       [default: 60, or a quarter of the run if shorter]
    --rate <HZ>        Engine sample rate the resampler targets [default: 48000]
    --process <PID>    Capture that process tree's audio (process loopback) instead of
                       the whole default render endpoint
    --no-keepalive     Don't play silence on the default render endpoint (without it,
                       loopback delivers no packets while nothing else plays)
    --json             Print the final summary as one JSON object
    -h, --help         Print this help";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub minutes: f64,
    pub progress_s: f64,
    pub csv: PathBuf,
    settle_s: Option<f64>,
    pub rate: u32,
    pub process: Option<u32>,
    pub keepalive: bool,
    pub json: bool,
}

impl Args {
    /// Parses the arguments. `Ok(None)` means help was requested.
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut args = Args {
            minutes: 60.0,
            progress_s: 10.0,
            csv: PathBuf::from("drift.csv"),
            settle_s: None,
            rate: 48_000,
            process: None,
            keepalive: true,
            json: false,
        };
        while let Some(flag) = it.next() {
            let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
            match flag.as_str() {
                "-h" | "--help" => return Ok(None),
                "--json" => args.json = true,
                "--no-keepalive" => args.keepalive = false,
                "--minutes" => args.minutes = positive(&value("--minutes")?, "--minutes")?,
                "--progress" => args.progress_s = positive(&value("--progress")?, "--progress")?,
                "--settle" => {
                    let v: f64 = parse_num(&value("--settle")?)?;
                    if !v.is_finite() || v < 0.0 {
                        return Err("--settle must be a finite number >= 0".into());
                    }
                    args.settle_s = Some(v);
                }
                "--csv" => args.csv = PathBuf::from(value("--csv")?),
                "--rate" => {
                    args.rate = parse_num(&value("--rate")?)?;
                    if !(8_000..=384_000).contains(&args.rate) {
                        return Err("--rate must be between 8000 and 384000".into());
                    }
                }
                "--process" => {
                    let pid: u32 = parse_num(&value("--process")?)?;
                    if pid == 0 {
                        return Err("--process needs a real process id".into());
                    }
                    args.process = Some(pid);
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(Some(args))
    }

    /// The run length in seconds.
    pub fn seconds(&self) -> f64 {
        self.minutes * 60.0
    }

    /// Seconds after the start before residual statistics count.
    pub fn settle_s(&self) -> f64 {
        self.settle_s
            .unwrap_or_else(|| (self.seconds() / 4.0).min(60.0))
    }
}

fn parse_num<T: std::str::FromStr>(s: &str) -> Result<T, String> {
    s.parse()
        .map_err(|_| format!("`{s}` is not a valid number"))
}

fn positive(s: &str, name: &str) -> Result<f64, String> {
    let v: f64 = parse_num(s)?;
    if !v.is_finite() || v <= 0.0 {
        return Err(format!("{name} must be a finite number > 0"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>, String> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn defaults() {
        let a = parse(&[]).unwrap().unwrap();
        assert_eq!(a.minutes, 60.0);
        assert_eq!(a.settle_s(), 60.0);
        assert_eq!(a.rate, 48_000);
        assert!(a.keepalive && !a.json && a.process.is_none());
        assert_eq!(a.csv, PathBuf::from("drift.csv"));
    }

    #[test]
    fn smoke_run_settles_early() {
        let a = parse(&["--minutes", "2", "--json"]).unwrap().unwrap();
        assert_eq!(a.seconds(), 120.0);
        assert_eq!(a.settle_s(), 30.0);
        assert!(a.json);
        let a = parse(&["--minutes", "2", "--settle", "5"])
            .unwrap()
            .unwrap();
        assert_eq!(a.settle_s(), 5.0);
    }

    #[test]
    fn all_options() {
        let a = parse(&[
            "--progress",
            "5",
            "--csv",
            "x.csv",
            "--rate",
            "44100",
            "--process",
            "1234",
            "--no-keepalive",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.progress_s, 5.0);
        assert_eq!(a.csv, PathBuf::from("x.csv"));
        assert_eq!(a.rate, 44_100);
        assert_eq!(a.process, Some(1234));
        assert!(!a.keepalive);
    }

    #[test]
    fn usage_lists_every_option() {
        for flag in [
            "--minutes",
            "--progress",
            "--csv",
            "--settle",
            "--rate",
            "--process",
            "--no-keepalive",
            "--json",
            "--help",
        ] {
            assert!(USAGE.contains(flag), "{flag}");
        }
    }

    #[test]
    fn errors_and_help() {
        assert_eq!(parse(&["--help"]).unwrap(), None);
        assert!(parse(&["--minutes"]).is_err());
        assert!(parse(&["--minutes", "0"]).is_err());
        assert!(parse(&["--minutes", "NaN"]).is_err());
        assert!(parse(&["--settle", "-1"]).is_err());
        assert!(parse(&["--rate", "1000"]).is_err());
        assert!(parse(&["--process", "0"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
