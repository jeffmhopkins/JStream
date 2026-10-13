//! Latency samples, percentiles and the final report (text and JSON).

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

/// Everything the maintainer pastes back.
pub struct Report {
    pub adapter_name: String,
    pub adapter_vendor_id: u32,
    pub adapter_device_id: u32,
    pub adapter_luid: String,
    pub driver_version: String,
    pub wgpu_driver: String,
    pub wgpu_driver_info: String,
    pub adapter_selection: String,
    pub encoder_name: String,
    pub encoder_d3d11_aware: bool,
    pub encoder_async: bool,
    pub display_size: (u32, u32),
    pub encode_size: (u32, u32),
    pub target_fps: u32,
    pub bitrate_kbps: u32,
    pub seconds_requested: f64,
    pub seconds_measured: f64,
    pub ticks: u64,
    pub wgc_frames: u64,
    pub frames_submitted: u64,
    pub frames_encoded: u64,
    /// Packets out by the end of the measured window (the rest came from the final drain).
    pub frames_encoded_in_window: u64,
    pub dropped_late_ticks: u64,
    pub dropped_encoder_busy: u64,
    pub dropped_ring_full: u64,
    pub convert_ms: Summary,
    pub encode_ms: Summary,
    pub tick_work_ms: Summary,
    pub cpu_percent_of_one_core: f64,
    pub cpu_percent_of_machine: f64,
    pub logical_cpus: u32,
    pub output_path: String,
    pub output_bytes: u64,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn to_json(&self) -> String {
        let warnings: Vec<String> = self.warnings.iter().map(|w| s(w)).collect();
        format!(
            concat!(
                "{{\"spike\":\"S1\",",
                "\"adapter\":{{\"name\":{},\"vendor_id\":\"0x{:04x}\",\"device_id\":\"0x{:04x}\",",
                "\"luid\":{},\"driver_version\":{},\"wgpu_driver\":{},\"wgpu_driver_info\":{},",
                "\"selection\":{}}},",
                "\"encoder\":{{\"name\":{},\"d3d11_aware\":{},\"async\":{}}},",
                "\"display_size\":[{},{}],\"encode_size\":[{},{}],",
                "\"target_fps\":{},\"bitrate_kbps\":{},",
                "\"seconds_requested\":{},\"seconds_measured\":{},",
                "\"ticks\":{},\"wgc_frames\":{},\"capture_fps\":{},",
                "\"frames_submitted\":{},\"frames_encoded\":{},\"frames_encoded_in_window\":{},\"encoded_fps\":{},",
                "\"dropped\":{{\"late_ticks\":{},\"encoder_busy\":{},\"ring_full\":{}}},",
                "\"convert_latency_ms\":{},\"encode_latency_ms\":{},\"tick_cpu_work_ms\":{},",
                "\"cpu\":{{\"percent_of_one_core\":{},\"percent_of_machine\":{},\"logical_cpus\":{}}},",
                "\"output\":{{\"path\":{},\"bytes\":{}}},",
                "\"warnings\":[{}]}}"
            ),
            s(&self.adapter_name),
            self.adapter_vendor_id,
            self.adapter_device_id,
            s(&self.adapter_luid),
            s(&self.driver_version),
            s(&self.wgpu_driver),
            s(&self.wgpu_driver_info),
            s(&self.adapter_selection),
            s(&self.encoder_name),
            self.encoder_d3d11_aware,
            self.encoder_async,
            self.display_size.0,
            self.display_size.1,
            self.encode_size.0,
            self.encode_size.1,
            self.target_fps,
            self.bitrate_kbps,
            num(self.seconds_requested),
            num(self.seconds_measured),
            self.ticks,
            self.wgc_frames,
            num(self.capture_fps()),
            self.frames_submitted,
            self.frames_encoded,
            self.frames_encoded_in_window,
            num(self.encoded_fps()),
            self.dropped_late_ticks,
            self.dropped_encoder_busy,
            self.dropped_ring_full,
            self.convert_ms.json(),
            self.encode_ms.json(),
            self.tick_work_ms.json(),
            num(self.cpu_percent_of_one_core),
            num(self.cpu_percent_of_machine),
            self.logical_cpus,
            s(&self.output_path),
            self.output_bytes,
            warnings.join(","),
        )
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let mut line = |l: String| {
            out.push_str(&l);
            out.push('\n');
        };
        line(format!(
            "adapter:        {} (vendor 0x{:04x}, device 0x{:04x}, LUID {})",
            self.adapter_name, self.adapter_vendor_id, self.adapter_device_id, self.adapter_luid
        ));
        line(format!(
            "driver:         {} (wgpu: {} / {})",
            self.driver_version, self.wgpu_driver, self.wgpu_driver_info
        ));
        line(format!("selection:      {}", self.adapter_selection));
        line(format!(
            "encoder MFT:    {} (D3D11-aware: {}, async: {})",
            self.encoder_name, self.encoder_d3d11_aware, self.encoder_async
        ));
        line(format!(
            "display {}x{} -> encode {}x{} @ {} fps target, {} kbit/s CBR",
            self.display_size.0,
            self.display_size.1,
            self.encode_size.0,
            self.encode_size.1,
            self.target_fps,
            self.bitrate_kbps
        ));
        line(format!(
            "measured:       {:.2} s, {} ticks",
            self.seconds_measured, self.ticks
        ));
        line(format!(
            "capture:        {} WGC frames = {:.2} fps (WGC only delivers frames when the screen changes)",
            self.wgc_frames,
            self.capture_fps()
        ));
        line(format!(
            "encode:         {} submitted, {} encoded ({} in window) = {:.2} fps",
            self.frames_submitted,
            self.frames_encoded,
            self.frames_encoded_in_window,
            self.encoded_fps()
        ));
        line(format!(
            "dropped ticks:  {} late, {} encoder busy, {} NV12 ring full",
            self.dropped_late_ticks, self.dropped_encoder_busy, self.dropped_ring_full
        ));
        line(format!("convert:        {}", self.convert_ms.text()));
        line(format!("encode:         {}", self.encode_ms.text()));
        line(format!("tick CPU work:  {}", self.tick_work_ms.text()));
        line(format!(
            "process CPU:    {:.1}% of one core, {:.2}% of {} logical CPUs",
            self.cpu_percent_of_one_core, self.cpu_percent_of_machine, self.logical_cpus
        ));
        line(format!(
            "output:         {} ({} bytes)",
            self.output_path, self.output_bytes
        ));
        for w in &self.warnings {
            line(format!("warning:        {w}"));
        }
        out
    }

    fn capture_fps(&self) -> f64 {
        self.wgc_frames as f64 / self.seconds_measured.max(f64::EPSILON)
    }

    fn encoded_fps(&self) -> f64 {
        self.frames_encoded_in_window as f64 / self.seconds_measured.max(f64::EPSILON)
    }
}

/// A JSON number; non-finite values become `null`.
fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.4}")
    } else {
        "null".into()
    }
}

/// A JSON string literal.
fn s(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
