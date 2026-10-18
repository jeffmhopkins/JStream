//! Latency samples, percentiles and the report for one mode (text and JSON).

/// Durations in milliseconds.
#[derive(Default)]
pub struct Samples(Vec<f64>);

impl Samples {
    pub fn push(&mut self, ms: f64) {
        self.0.push(ms);
    }

    pub fn summary(&self) -> Summary {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        // Nearest-rank percentile.
        let pct = |p: f64| -> f64 {
            if v.is_empty() {
                return f64::NAN;
            }
            let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
            v[rank.clamp(1, v.len()) - 1]
        };
        Summary {
            count: v.len(),
            mean: if v.is_empty() {
                f64::NAN
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            },
            p50: pct(50.0),
            p95: pct(95.0),
            p99: pct(99.0),
            max: v.last().copied().unwrap_or(f64::NAN),
        }
    }
}

pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Summary {
    fn json(&self) -> String {
        format!(
            "{{\"count\":{},\"mean\":{},\"p50\":{},\"p95\":{},\"p99\":{},\"max\":{}}}",
            self.count,
            num(self.mean),
            num(self.p50),
            num(self.p95),
            num(self.p99),
            num(self.max)
        )
    }

    fn text(&self) -> String {
        format!(
            "p50 {:.3} / p95 {:.3} / p99 {:.3} / max {:.3} ms (mean {:.3}, n={})",
            self.p50, self.p95, self.p99, self.max, self.mean, self.count
        )
    }
}

/// Everything the maintainer pastes back, for one mode.
pub struct Report {
    pub mode: &'static str,
    pub macos: String,
    pub machine: String,
    pub adapter_name: String,
    pub wgpu_driver: String,
    pub same_mtl_device: bool,
    pub encoder_hardware: bool,
    pub size: (u32, u32),
    pub target_fps: u32,
    pub load: u32,
    pub seconds_requested: f64,
    pub seconds_measured: f64,
    pub ticks: u64,
    pub frames_submitted: u64,
    pub frames_checked: u64,
    /// The engine read the producer's previous frame: the import wasn't synchronised.
    pub stale_import: u64,
    /// The consumer saw the engine's previous frame: the hand-over wasn't synchronised.
    pub stale_export: u64,
    /// A probe value that is neither this frame's nor an earlier one's: a torn or
    /// unwritten frame.
    pub garbled: u64,
    pub sync_timeouts: u64,
    pub dropped_late_ticks: u64,
    pub dropped_ring_full: u64,
    pub frames_encoded: u64,
    /// Producer submit to the consumer holding a finished frame.
    pub latency_ms: Summary,
    /// CPU time the tick thread spends per frame, waits included.
    pub tick_work_ms: Summary,
    pub cpu_percent_of_one_core: f64,
    pub output_path: String,
    pub output_bytes: u64,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn to_json(&self) -> String {
        let warnings: Vec<String> = self.warnings.iter().map(|w| s(w)).collect();
        format!(
            concat!(
                "{{\"spike\":\"S6\",\"mode\":{},\"macos\":{},\"machine\":{},",
                "\"adapter\":{{\"name\":{},\"wgpu_driver\":{},\"same_mtl_device\":{}}},",
                "\"encoder_hardware\":{},\"size\":[{},{}],\"target_fps\":{},\"load\":{},",
                "\"seconds_requested\":{},\"seconds_measured\":{},",
                "\"ticks\":{},\"frames_submitted\":{},\"frames_checked\":{},",
                "\"stale_import\":{},\"stale_export\":{},\"garbled\":{},\"sync_timeouts\":{},",
                "\"dropped\":{{\"late_ticks\":{},\"ring_full\":{}}},\"frames_encoded\":{},",
                "\"latency_ms\":{},\"tick_work_ms\":{},",
                "\"cpu_percent_of_one_core\":{},",
                "\"output\":{{\"path\":{},\"bytes\":{}}},\"warnings\":[{}]}}"
            ),
            s(self.mode),
            s(&self.macos),
            s(&self.machine),
            s(&self.adapter_name),
            s(&self.wgpu_driver),
            self.same_mtl_device,
            self.encoder_hardware,
            self.size.0,
            self.size.1,
            self.target_fps,
            self.load,
            num(self.seconds_requested),
            num(self.seconds_measured),
            self.ticks,
            self.frames_submitted,
            self.frames_checked,
            self.stale_import,
            self.stale_export,
            self.garbled,
            self.sync_timeouts,
            self.dropped_late_ticks,
            self.dropped_ring_full,
            self.frames_encoded,
            self.latency_ms.json(),
            self.tick_work_ms.json(),
            num(self.cpu_percent_of_one_core),
            s(&self.output_path),
            self.output_bytes,
            warnings.join(","),
        )
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "S6 {mode}: {adapter} ({driver}), macOS {macos} on {machine}\n\
             size {w}x{h} at {fps} fps, load {load}, {secs:.1} s measured\n\
             frames: {submitted} submitted, {checked} checked, {encoded} encoded \
             (VideoToolbox {hw})\n\
             stale import {si}, stale export {se}, garbled {g}, sync timeouts {st}\n\
             dropped: {late} late ticks, {ring} ring full\n\
             latency {lat}\n\
             tick work {work}\n\
             cpu {cpu:.1}% of one core\n\
             output {path} ({bytes} bytes)",
            mode = self.mode,
            adapter = self.adapter_name,
            driver = self.wgpu_driver,
            macos = self.macos,
            machine = self.machine,
            w = self.size.0,
            h = self.size.1,
            fps = self.target_fps,
            load = self.load,
            secs = self.seconds_measured,
            submitted = self.frames_submitted,
            checked = self.frames_checked,
            encoded = self.frames_encoded,
            hw = if self.encoder_hardware {
                "hardware"
            } else {
                "software"
            },
            si = self.stale_import,
            se = self.stale_export,
            g = self.garbled,
            st = self.sync_timeouts,
            late = self.dropped_late_ticks,
            ring = self.dropped_ring_full,
            lat = self.latency_ms.text(),
            work = self.tick_work_ms.text(),
            cpu = self.cpu_percent_of_one_core,
            path = self.output_path,
            bytes = self.output_bytes,
        );
        if !self.same_mtl_device {
            out.push_str("\nnote: the producer and the engine are on different MTLDevices");
        }
        for w in &self.warnings {
            out.push_str("\nwarning: ");
            out.push_str(w);
        }
        out
    }
}

/// A JSON number, or `null` for NaN and infinities.
fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".into()
    }
}

/// A JSON string.
fn s(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
