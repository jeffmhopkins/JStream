//! The CSV time series, the progress line and the final report (text and JSON).

use crate::stream::{Snapshot, StreamInfo};
use crate::track::Stat;

/// The residual bound the spike checks, in milliseconds.
pub const BOUND_MS: f64 = 1.0;

/// Drift of stream `a` relative to stream `b`, in ppm, from each one's drift against
/// QPC: positive when `a` delivers more samples per second of `b` than nominal.
pub fn relative_ppm(a: f64, b: f64) -> f64 {
    ((1.0 + a * 1e-6) / (1.0 + b * 1e-6) - 1.0) * 1e6
}

pub const CSV_HEADER: &str = "t_s,\
mic_packets,loop_packets,\
mic_drift_ppm,loop_drift_ppm,rel_drift_ppm,\
mic_drift_stderr_ppm,loop_drift_stderr_ppm,\
mic_offset_ms,loop_offset_ms,mic_offset_raw_ms,loop_offset_raw_ms,\
mic_jitter_ms,loop_jitter_ms,\
mic_correction_ppm,loop_correction_ppm,\
mic_residual_ms,loop_residual_ms,mic_residual_raw_ms,loop_residual_raw_ms,rel_residual_ms,\
mic_glitches,loop_glitches,mic_resets,loop_resets";

/// One CSV row at `t` seconds into the run. Unknown values are empty.
pub fn csv_row(t: f64, mic: &Snapshot, lb: &Snapshot) -> String {
    let rel = match (mic.drift_ppm, lb.drift_ppm) {
        (Some(a), Some(b)) => Some(relative_ppm(a, b)),
        _ => None,
    };
    let fields = [
        format!("{t:.3}"),
        mic.packets.to_string(),
        lb.packets.to_string(),
        c(mic.drift_ppm),
        c(lb.drift_ppm),
        c(rel),
        c(mic.drift_stderr_ppm),
        c(lb.drift_stderr_ppm),
        c(mic.offset_ms),
        c(lb.offset_ms),
        c(mic.offset_raw_ms),
        c(lb.offset_raw_ms),
        c(mic.jitter_ms),
        c(lb.jitter_ms),
        c(Some(mic.correction_ppm)),
        c(Some(lb.correction_ppm)),
        c(mic.residual_ms),
        c(lb.residual_ms),
        c(mic.residual_raw_ms),
        c(lb.residual_raw_ms),
        c(relative_residual(mic, lb)),
        mic.glitches().to_string(),
        lb.glitches().to_string(),
        mic.resets().to_string(),
        lb.resets().to_string(),
    ];
    fields.join(",")
}

/// Mic minus loopback smoothed residual, ms: how far apart the two resampled streams
/// are on the engine time line (both are steered to QPC).
pub fn relative_residual(mic: &Snapshot, lb: &Snapshot) -> Option<f64> {
    Some(mic.residual_ms? - lb.residual_ms?)
}

/// The progress line printed every few seconds.
pub fn progress_line(t: f64, total: f64, mic: &Snapshot, lb: &Snapshot) -> String {
    let rel = match (mic.drift_ppm, lb.drift_ppm) {
        (Some(a), Some(b)) => format!("{:+.2}", relative_ppm(a, b)),
        _ => "--".into(),
    };
    format!(
        "[{t:>6.0}/{total:.0} s] mic {} ppm, off {} ms, resid {} ms | loop {} ppm, off {} ms, resid {} ms | rel {rel} ppm | glitches {}/{}",
        p(mic.drift_ppm, 2),
        p(mic.offset_ms, 2),
        p(mic.residual_ms, 3),
        p(lb.drift_ppm, 2),
        p(lb.offset_ms, 2),
        p(lb.residual_ms, 3),
        mic.glitches(),
        lb.glitches(),
    )
}

/// One stream in the report.
pub struct StreamReport {
    pub info: Option<StreamInfo>,
    pub snap: Snapshot,
    pub mmcss: bool,
    pub wait_timeouts: u64,
    pub error: Option<String>,
}

impl StreamReport {
    /// Whether the residual was measured and stayed within [`BOUND_MS`].
    fn within_bound(&self) -> Option<bool> {
        (self.snap.residual.n > 0).then(|| self.snap.residual.max() <= BOUND_MS)
    }

    fn json(&self) -> String {
        let s = &self.snap;
        let info = match &self.info {
            Some(i) => format!(
                concat!(
                    "\"role\":{},\"device\":{},\"device_id\":{},\"rate\":{},\"channels\":{},",
                    "\"format\":{},\"bits\":{},\"valid_bits\":{},",
                    "\"buffer_frames\":{},\"period_ms\":{},"
                ),
                js(i.role.key()),
                js(&i.device_name),
                js(&i.device_id),
                i.rate,
                i.channels,
                js(i.format.name()),
                i.bits,
                i.valid_bits,
                i.buffer_frames,
                num(i.period_hns as f64 / 1e4),
            ),
            None => String::new(),
        };
        format!(
            concat!(
                "{{{}\"mmcss\":{},\"error\":{},",
                "\"packets\":{},\"frames\":{},",
                "\"glitches\":{{\"total\":{},\"discontinuity_flags\":{},\"first_packet_discontinuity\":{},",
                "\"gaps\":{},\"gap_frames\":{},\"timestamp_errors\":{},\"silent_packets\":{},",
                "\"outliers\":{},\"resets\":{{\"backwards\":{},\"gap_too_large\":{},",
                "\"gap_time_mismatch\":{},\"outlier_run\":{}}},\"wait_timeouts\":{}}},",
                "\"drift\":{{\"ppm\":{},\"stderr_ppm\":{},\"jitter_ms\":{},\"offset_ms\":{},",
                "\"offset_raw_ms\":{},\"points\":{},\"segments\":{},\"span_s\":{},",
                "\"mapping_residual_ms\":{{\"n\":{},\"max_abs\":{},\"rms\":{}}}}},",
                "\"resampler\":{{\"correction_ppm\":{},\"reanchors\":{},",
                "\"residual_ms\":{{\"n\":{},\"max_abs\":{},\"rms\":{}}},",
                "\"residual_raw_ms\":{{\"n\":{},\"max_abs\":{},\"rms\":{}}},",
                "\"within_1ms\":{}}},",
                "\"packet_cpu_us\":{{\"mean\":{},\"max\":{}}}}}"
            ),
            info,
            self.mmcss,
            self.error.as_deref().map_or("null".into(), js),
            s.packets,
            s.frames,
            s.glitches(),
            s.discontinuities,
            s.first_packet_discontinuity,
            s.gaps,
            s.gap_frames,
            s.timestamp_errors,
            s.silent_packets,
            s.outliers,
            s.resets_backwards,
            s.resets_gap_too_large,
            s.resets_gap_time_mismatch,
            s.resets_outliers,
            self.wait_timeouts,
            opt(s.drift_ppm),
            opt(s.drift_stderr_ppm),
            opt(s.jitter_ms),
            opt(s.offset_ms),
            opt(s.offset_raw_ms),
            s.fit_points,
            s.fit_segments,
            num(s.fit_span_s),
            s.mapping_ms.n,
            num(s.mapping_ms.max()),
            num(s.mapping_ms.rms()),
            num(s.correction_ppm),
            s.reanchors,
            s.residual.n,
            num(s.residual.max()),
            num(s.residual.rms()),
            s.residual_raw.n,
            num(s.residual_raw.max()),
            num(s.residual_raw.rms()),
            self.within_bound().map_or("null".into(), |b| b.to_string()),
            num(s.process_us_mean),
            num(s.process_us_max),
        )
    }

    fn text(&self, name: &str, out: &mut String) {
        let s = &self.snap;
        let mut line = |l: String| {
            out.push_str(&l);
            out.push('\n');
        };
        match &self.info {
            Some(i) => line(format!(
                "{name}: {} ({} Hz, {} ch, {} {}/{} bit, buffer {} frames, period {:.2} ms, MMCSS {})",
                i.device_name,
                i.rate,
                i.channels,
                i.format.name(),
                i.bits,
                i.valid_bits,
                i.buffer_frames,
                i.period_hns as f64 / 1e4,
                self.mmcss
            )),
            None => line(format!("{name}: not opened")),
        }
        if let Some(e) = &self.error {
            line(format!("  error: {e}"));
        }
        line(format!(
            "  packets {}, frames {}, glitches {} (discontinuity flags {}, gaps {} = {} frames, timestamp errors {}, resets {}, outliers {}), silent packets {}, wait timeouts {}",
            s.packets,
            s.frames,
            s.glitches(),
            s.discontinuities,
            s.gaps,
            s.gap_frames,
            s.timestamp_errors,
            s.resets(),
            s.outliers,
            s.silent_packets,
            self.wait_timeouts
        ));
        line(format!(
            "  drift {} ppm (stderr {}), accumulated {} ms over {:.0} s (raw {} ms), timestamp jitter {} ms, {} points in {} segment(s)",
            p(s.drift_ppm, 3),
            p(s.drift_stderr_ppm, 3),
            p(s.offset_ms, 3),
            s.fit_span_s,
            p(s.offset_raw_ms, 3),
            p(s.jitter_ms, 3),
            s.fit_points,
            s.fit_segments
        ));
        line(format!(
            "  position->QPC mapping vs raw stamps: max {:.3} ms, rms {:.3} ms (n={})",
            s.mapping_ms.max(),
            s.mapping_ms.rms(),
            s.mapping_ms.n
        ));
        line(format!(
            "  resampler: correction {:+.2} ppm, residual max {:.3} ms rms {:.3} ms (raw max {:.3} rms {:.3}), n={}, within ±{BOUND_MS} ms: {}",
            s.correction_ppm,
            s.residual.max(),
            s.residual.rms(),
            s.residual_raw.max(),
            s.residual_raw.rms(),
            s.residual.n,
            self.within_bound().map_or("n/a".into(), |b| b.to_string())
        ));
        line(format!(
            "  per-packet CPU: mean {:.1} us, max {:.1} us",
            s.process_us_mean, s.process_us_max
        ));
    }
}

/// Everything the maintainer pastes back.
pub struct Report {
    pub windows: String,
    pub seconds_requested: f64,
    pub seconds_measured: f64,
    pub interrupted: bool,
    pub settle_s: f64,
    pub engine_rate: u32,
    pub keepalive: Option<String>,
    pub keepalive_error: Option<String>,
    pub mic: StreamReport,
    pub loopback: StreamReport,
    /// Mic-minus-loopback smoothed residual sampled once a second after settling, ms.
    pub relative_residual: Stat,
    pub cpu_percent_of_one_core: f64,
    pub cpu_percent_of_machine: f64,
    pub logical_cpus: u32,
    pub csv_path: String,
    pub csv_rows: u64,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn relative_drift_ppm(&self) -> Option<f64> {
        Some(relative_ppm(
            self.mic.snap.drift_ppm?,
            self.loopback.snap.drift_ppm?,
        ))
    }

    /// True when both streams' smoothed residuals stayed within the bound.
    pub fn verdict(&self) -> Option<bool> {
        Some(self.mic.within_bound()? && self.loopback.within_bound()?)
    }

    pub fn to_json(&self) -> String {
        let warnings: Vec<String> = self.warnings.iter().map(|w| js(w)).collect();
        format!(
            concat!(
                "{{\"spike\":\"S3\",\"windows\":{},",
                "\"seconds_requested\":{},\"seconds_measured\":{},\"interrupted\":{},",
                "\"settle_s\":{},\"engine_rate\":{},",
                "\"keepalive\":{{\"device\":{},\"error\":{}}},",
                "\"mic\":{},\"loopback\":{},",
                "\"relative\":{{\"drift_ppm\":{},\"residual_ms\":{{\"n\":{},\"max_abs\":{},\"rms\":{}}}}},",
                "\"resampler_within_1ms\":{},",
                "\"cpu\":{{\"percent_of_one_core\":{},\"percent_of_machine\":{},\"logical_cpus\":{}}},",
                "\"csv\":{{\"path\":{},\"rows\":{}}},",
                "\"warnings\":[{}]}}"
            ),
            js(&self.windows),
            num(self.seconds_requested),
            num(self.seconds_measured),
            self.interrupted,
            num(self.settle_s),
            self.engine_rate,
            self.keepalive.as_deref().map_or("null".into(), js),
            self.keepalive_error.as_deref().map_or("null".into(), js),
            self.mic.json(),
            self.loopback.json(),
            opt(self.relative_drift_ppm()),
            self.relative_residual.n,
            num(self.relative_residual.max()),
            num(self.relative_residual.rms()),
            self.verdict().map_or("null".into(), |b| b.to_string()),
            num(self.cpu_percent_of_one_core),
            num(self.cpu_percent_of_machine),
            self.logical_cpus,
            js(&self.csv_path),
            self.csv_rows,
            warnings.join(","),
        )
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("windows:   {}\n", self.windows));
        out.push_str(&format!(
            "measured:  {:.1} s of {:.1} s requested{}; residual statistics after {:.0} s; engine {} Hz\n",
            self.seconds_measured,
            self.seconds_requested,
            if self.interrupted { " (interrupted)" } else { "" },
            self.settle_s,
            self.engine_rate
        ));
        match (&self.keepalive, &self.keepalive_error) {
            (_, Some(e)) => out.push_str(&format!("keepalive: failed: {e}\n")),
            (Some(d), None) => out.push_str(&format!("keepalive: silence on {d}\n")),
            (None, None) => out.push_str("keepalive: off\n"),
        }
        self.mic.text("mic", &mut out);
        self.loopback.text("loopback", &mut out);
        out.push_str(&format!(
            "relative:  mic vs loopback drift {} ppm; resampled offset max {:.3} ms rms {:.3} ms (n={})\n",
            p(self.relative_drift_ppm(), 3),
            self.relative_residual.max(),
            self.relative_residual.rms(),
            self.relative_residual.n
        ));
        out.push_str(&format!(
            "verdict:   resampler residual within ±{BOUND_MS} ms on both streams: {}\n",
            self.verdict().map_or("n/a".into(), |b| b.to_string())
        ));
        out.push_str(&format!(
            "cpu:       {:.2}% of one core, {:.3}% of {} logical CPUs\n",
            self.cpu_percent_of_one_core, self.cpu_percent_of_machine, self.logical_cpus
        ));
        out.push_str(&format!(
            "csv:       {} ({} rows)\n",
            self.csv_path, self.csv_rows
        ));
        for w in &self.warnings {
            out.push_str(&format!("warning:   {w}\n"));
        }
        out
    }
}

/// A CSV field: empty when unknown.
fn c(v: Option<f64>) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:.6}"),
        _ => String::new(),
    }
}

/// A signed text value with `prec` decimals, or `--`.
fn p(v: Option<f64>, prec: usize) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:+.prec$}"),
        _ => "--".into(),
    }
}

/// A JSON number; non-finite values become `null`.
fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.6}")
    } else {
        "null".into()
    }
}

fn opt(v: Option<f64>) -> String {
    v.map_or("null".into(), num)
}

/// A JSON string literal.
fn js(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for ch in v.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{Role, SampleFormat};

    #[test]
    fn relative_drift() {
        assert!((relative_ppm(50.0, 50.0)).abs() < 1e-9);
        // Close to a - b for small values.
        let r = relative_ppm(30.0, -20.0);
        assert!((r - 50.0).abs() < 0.01, "{r}");
    }

    fn stream(max_ms: f64) -> StreamReport {
        let mut snap = Snapshot {
            packets: 10,
            drift_ppm: Some(12.5),
            residual_ms: Some(0.1),
            ..Snapshot::default()
        };
        snap.residual.add(max_ms);
        StreamReport {
            info: Some(StreamInfo {
                role: Role::Mic,
                device_name: "Mic \"USB\"\\x".into(),
                device_id: "{0.0.1}".into(),
                rate: 48_000,
                channels: 2,
                format: SampleFormat::F32,
                bits: 32,
                valid_bits: 32,
                buffer_frames: 4800,
                period_hns: 100_000,
            }),
            snap,
            mmcss: true,
            wait_timeouts: 0,
            error: None,
        }
    }

    fn report(mic_max: f64) -> Report {
        Report {
            windows: "Windows 11 Pro 24H2 (build 26100.4061)".into(),
            seconds_requested: 120.0,
            seconds_measured: 120.0,
            interrupted: false,
            settle_s: 30.0,
            engine_rate: 48_000,
            keepalive: Some("Speakers".into()),
            keepalive_error: None,
            mic: stream(mic_max),
            loopback: stream(0.2),
            relative_residual: Stat::default(),
            cpu_percent_of_one_core: 3.0,
            cpu_percent_of_machine: 0.2,
            logical_cpus: 16,
            csv_path: "drift.csv".into(),
            csv_rows: 120,
            warnings: vec!["a\nb".into()],
        }
    }

    #[test]
    fn json_is_balanced_and_escaped() {
        let j = report(0.5).to_json();
        assert!(j.starts_with("{\"spike\":\"S3\""));
        assert!(j.contains("\"role\":\"mic\""));
        assert!(j.contains("\"device\":\"Mic \\\"USB\\\"\\\\x\""));
        assert!(j.contains("\"resampler_within_1ms\":true"));
        assert!(j.contains("\"warnings\":[\"a\\nb\"]"));
        // Braces and brackets balance outside strings.
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for ch in j.chars() {
            match (in_str, esc, ch) {
                (true, true, _) => esc = false,
                (true, false, '\\') => esc = true,
                (true, false, '"') => in_str = false,
                (false, _, '"') => in_str = true,
                (false, _, '{' | '[') => depth += 1,
                (false, _, '}' | ']') => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0);
        }
        assert_eq!(depth, 0);
        assert!(!j.contains("NaN") && !j.contains("inf"));
    }

    #[test]
    fn verdict_fails_over_bound() {
        assert_eq!(report(0.9).verdict(), Some(true));
        assert_eq!(report(1.5).verdict(), Some(false));
        let mut r = report(0.5);
        r.loopback.snap.residual = Stat::default();
        assert_eq!(r.verdict(), None);
        assert!(r.to_text().contains("verdict:"));
    }

    #[test]
    fn csv_row_matches_header() {
        let r = report(0.5);
        let row = csv_row(1.0, &r.mic.snap, &Snapshot::default());
        assert_eq!(row.split(',').count(), CSV_HEADER.split(',').count());
        assert!(row.starts_with("1.000,10,0,12.500000,,"));
        let line = progress_line(10.0, 120.0, &r.mic.snap, &r.loopback.snap);
        assert!(line.contains("rel +0.00 ppm"), "{line}");
    }
}
