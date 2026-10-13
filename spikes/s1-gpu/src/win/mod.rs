//! The Windows pipeline and its frame loop.
//!
//! Per tick (default 60 Hz), on one thread:
//! 1. take the newest WGC frame, if any, and `CopyResource` it into the shared BGRA texture
//!    (the one GPU copy ADR-0004 expects, since WGC pool textures can't be shared);
//! 2. D3D11 `Signal(fence, a)` + `Flush`;
//! 3. wgpu: `Wait(fence, a)`, two render passes write the NV12 planes of a ring slot,
//!    transitions back to COMMON, `Signal(fence, b)` (staged through wgpu-hal);
//! 4. D3D11 `Wait(fence, b)`, then the slot's texture goes to the MF encoder as an
//!    `IMFDXGIBuffer` sample.
//!
//! A tick with no new WGC frame re-encodes the last one (ARCH: a missing source frame
//! repeats the last one), so the pipeline runs at the target rate on a static desktop.

mod capture;
mod convert;
mod crash;
mod d3d;
mod encode;

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::err::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, FILETIME, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE};
use windows::Win32::Graphics::Direct3D12::ID3D12Fence;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetProcessTimes, WaitForSingleObject,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};

use crate::cli::{Args, Size};
use crate::stats::{Report, Samples};
pub use crash::stage;

/// NV12 textures in flight between the converter and the encoder. Generous, so an encoder
/// that buffers a few inputs before its first output cannot stall the ring.
const RING: usize = 8;

pub fn run(args: &Args) -> Result<Report> {
    crash::install();
    stage(format_args!("start, {args:?}"));
    // SAFETY: called once on this thread before any WinRT/COM use. An "already
    // initialised" result is fine.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    // SAFETY: plain call; paired with MFShutdown below.
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.context("MFStartup")?;
    let result = run_pipeline(args);
    // SAFETY: paired with the MFStartup above; every MF object was dropped in run_pipeline.
    let _ = unsafe { MFShutdown() };
    result
}

fn run_pipeline(args: &Args) -> Result<Report> {
    let monitor = capture::primary_monitor();
    stage("primary monitor found; creating the wgpu DX12 device");
    let gpu = convert::Gpu::new(monitor)?;
    let info = gpu.adapter.get_info();
    stage(format_args!(
        "wgpu device on {} ({})",
        info.name, gpu.selection
    ));
    let facts = d3d::adapter_facts(&gpu.dxgi_adapter)?;
    let d3d11 = d3d::create_d3d11(&gpu.dxgi_adapter)?;
    stage("D3D11 device created; starting Windows Graphics Capture");

    let capture = capture::Capture::start(&d3d11.device.clone().into(), monitor)?;
    let display = capture.size;
    stage(format_args!(
        "capture started, display {}x{}",
        display.0, display.1
    ));
    let encode_size = match args.size {
        Size::Native => (display.0 & !1, display.1 & !1),
        Size::Fixed(w, h) => (w, h),
    };

    // Shared resources.
    let fence = d3d::SharedFence::new(&d3d11.device, &gpu.d3d12)?;
    let source = d3d::SharedTexture::new(
        &d3d11.device,
        &gpu.d3d12,
        display.0,
        display.1,
        DXGI_FORMAT_B8G8R8A8_UNORM,
        D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
    )?;
    let source_wgpu = gpu.import_texture(
        source.d3d12.clone(),
        wgpu::TextureFormat::Bgra8Unorm,
        display,
        wgpu::TextureUsages::TEXTURE_BINDING,
        "capture (shared)",
    );
    let source_view = source_wgpu.create_view(&wgpu::TextureViewDescriptor::default());
    let mut ring = Vec::with_capacity(RING);
    for i in 0..RING {
        let shared = d3d::SharedTexture::new(
            &d3d11.device,
            &gpu.d3d12,
            encode_size.0,
            encode_size.1,
            DXGI_FORMAT_NV12,
            D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE,
        )
        .with_context(|| format!("NV12 ring slot {i}"))?;
        let texture = gpu.import_texture(
            shared.d3d12.clone(),
            wgpu::TextureFormat::NV12,
            encode_size,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
            "nv12 (shared)",
        );
        ring.push((shared, convert::Nv12Target::new(texture)));
    }
    let converter = convert::Converter::new(&gpu.device, &source_view);
    stage("shared textures imported and converter built; creating the encoder");

    let mut encoder = encode::Encoder::new(
        &d3d11.device.clone().into(),
        facts.luid,
        encode_size,
        args.fps,
        args.bitrate_kbps,
    )?;
    let out = BufWriter::new(
        File::create(&args.out).with_context(|| format!("create {}", args.out.display()))?,
    );

    // Convert latency: CPU submit -> D3D12 fence reached, measured off-thread so the frame
    // loop never blocks on the GPU.
    let (fence_tx, fence_rx) = mpsc::channel::<(u64, Instant)>();
    let watched: ID3D12Fence = fence.d3d12.clone();
    let watcher = thread::spawn(move || watch_fence(watched, fence_rx));

    let period = Duration::from_secs_f64(1.0 / f64::from(args.fps));
    let frame_100ns = 10_000_000 / i64::from(args.fps);
    let mut warnings = Vec::new();
    let mut packets = Vec::new();
    let mut sink = Sink {
        file: out,
        bytes: 0,
        packets: 0,
        encode_ms: Samples::default(),
        in_flight: VecDeque::new(),
        free_slots: (0..RING).rev().collect(),
        unmatched: 0,
    };
    let mut tick_work_ms = Samples::default();
    let mut fence_value = 0u64;
    let (mut ticks, mut wgc_frames, mut submitted) = (0u64, 0u64, 0u64);
    let (mut late, mut busy, mut ring_full) = (0u64, 0u64, 0u64);
    let mut size_warned = false;

    stage("encoder ready; frame loop starts");
    let cpu_start = process_cpu_time();
    let start = Instant::now();
    let end = start + Duration::from_secs_f64(args.seconds);
    let mut tick_no = 0u64;
    let mut next = start;

    while Instant::now() < end {
        // Wait for the tick, servicing the encoder meanwhile.
        loop {
            encoder.poll(&mut packets)?;
            sink.take(&mut packets)?;
            let now = Instant::now();
            if now >= next {
                break;
            }
            thread::sleep((next - now).min(Duration::from_millis(1)));
        }
        let work_start = Instant::now();
        let behind = ((work_start - next).as_secs_f64() / period.as_secs_f64()).floor() as u64;
        late += behind;
        tick_no += behind;
        next += period * (behind as u32 + 1);
        let this_tick = tick_no;
        tick_no += 1;
        ticks += 1;

        // 1. Capture: copy the newest WGC frame into the shared BGRA texture.
        let (frame, n) = capture.latest()?;
        wgc_frames += n;
        if let Some(frame) = frame {
            if frame.size != display && !size_warned {
                size_warned = true;
                warnings.push(format!(
                    "display content size changed to {:?}; frames keep the original size",
                    frame.size
                ));
            }
            // SAFETY: both textures live on this D3D11 device with identical size and
            // format (the pool was created at the display size in BGRA8).
            unsafe { d3d11.context.CopyResource(&source.d3d11, &frame.texture) };
        }

        // 2. Back-pressure: a free NV12 slot and an encoder that wants input.
        let Some(&slot) = sink.free_slots.last() else {
            ring_full += 1;
            continue;
        };
        if !encoder.can_accept() {
            busy += 1;
            continue;
        }
        let (shared, target) = &ring[slot];

        // 3. D3D11 -> D3D12: the copy is done before the conversion reads it.
        fence_value += 1;
        let copied = fence_value;
        fence_value += 1;
        let converted = fence_value;
        // SAFETY: plain calls on live COM objects; the fence is shared with D3D12.
        unsafe {
            d3d11.context.Signal(&fence.d3d11, copied)?;
            d3d11.context.Flush();
        }
        let commands = converter.record(&gpu.device, &source_wgpu, target);
        gpu.stage_fences(&fence.d3d12, copied, converted)?;
        gpu.queue.submit([commands]);
        let _ = fence_tx.send((converted, Instant::now()));
        // Lets wgpu retire finished submissions; never blocks.
        let _ = gpu.device.poll(wgpu::PollType::Poll);

        // 4. D3D12 -> D3D11: the encoder only reads the NV12 slot after conversion.
        // SAFETY: plain calls on live COM objects; the fence is shared with D3D12.
        unsafe {
            d3d11.context.Wait(&fence.d3d11, converted)?;
            d3d11.context.Flush();
        }
        let time = this_tick as i64 * frame_100ns;
        if submitted == 0 {
            stage("first frame converted; submitting it to the encoder");
        }
        encoder.submit(&shared.d3d11, time, frame_100ns)?;
        if submitted == 0 {
            stage("first frame submitted");
        }
        sink.free_slots.pop();
        sink.in_flight.push_back((time, Instant::now(), slot));
        submitted += 1;
        tick_work_ms.push(work_start.elapsed().as_secs_f64() * 1e3);
    }
    stage(format_args!(
        "frame loop done: {ticks} ticks, {submitted} submitted, {} packets",
        sink.packets
    ));
    let measured = start.elapsed().as_secs_f64();
    // Packets drained after this point are counted in frames_encoded but not in the fps.
    let encoded_in_window = sink.packets;
    let cpu_used = process_cpu_time() - cpu_start;

    encoder.drain(&mut packets, Duration::from_secs(3))?;
    sink.take(&mut packets)?;
    sink.file.flush().context("flush out.h264")?;
    capture.stop();
    drop(fence_tx);
    let convert_ms = watcher
        .join()
        .map_err(|_| crate::err::Error::msg("fence watcher panicked"))?;
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());

    warnings.append(&mut encoder.warnings);
    if sink.unmatched > 0 {
        warnings.push(format!(
            "{} encoder output(s) had no input with the same sample time; they freed no \
             NV12 slot and have no encode latency",
            sink.unmatched
        ));
    }
    let logical_cpus = thread::available_parallelism().map_or(1, |n| n.get() as u32);
    Ok(Report {
        adapter_name: facts.name,
        adapter_vendor_id: facts.vendor_id,
        adapter_device_id: facts.device_id,
        adapter_luid: format!("{:#x}", d3d::luid_u64(facts.luid)),
        driver_version: facts.driver_version,
        wgpu_driver: info.driver,
        wgpu_driver_info: info.driver_info,
        adapter_selection: gpu.selection.clone(),
        encoder_name: encoder.name.clone(),
        encoder_d3d11_aware: encoder.d3d11_aware,
        encoder_async: encoder.is_async,
        display_size: display,
        encode_size,
        target_fps: args.fps,
        bitrate_kbps: args.bitrate_kbps,
        seconds_requested: args.seconds,
        seconds_measured: measured,
        ticks,
        wgc_frames,
        frames_submitted: submitted,
        frames_encoded: sink.packets,
        frames_encoded_in_window: encoded_in_window,
        dropped_late_ticks: late,
        dropped_encoder_busy: busy,
        dropped_ring_full: ring_full,
        convert_ms: convert_ms.summary(),
        encode_ms: sink.encode_ms.summary(),
        tick_work_ms: tick_work_ms.summary(),
        cpu_percent_of_one_core: cpu_used / measured * 100.0,
        cpu_percent_of_machine: cpu_used / measured * 100.0 / f64::from(logical_cpus),
        logical_cpus,
        output_path: args.out.display().to_string(),
        output_bytes: sink.bytes,
        warnings,
    })
}

/// Writes encoded packets and matches them to their inputs by sample time, which frees the
/// input's NV12 slot and gives its encode latency.
struct Sink {
    file: BufWriter<File>,
    bytes: u64,
    packets: u64,
    encode_ms: Samples,
    /// (sample time, ProcessInput instant, ring slot) of inputs with no output yet.
    in_flight: VecDeque<(i64, Instant, usize)>,
    /// Ring slots not held by the encoder.
    free_slots: Vec<usize>,
    /// Outputs whose sample time matched no in-flight input.
    unmatched: u64,
}

impl Sink {
    fn take(&mut self, packets: &mut Vec<encode::Packet>) -> Result<()> {
        for p in packets.drain(..) {
            self.file.write_all(&p.data).context("write out.h264")?;
            self.bytes += p.data.len() as u64;
            self.packets += 1;
            let found = self
                .in_flight
                .iter()
                .position(|&(time, _, _)| time == p.time)
                .and_then(|i| self.in_flight.remove(i));
            match found {
                Some((_, t_in, slot)) => {
                    self.encode_ms.push(t_in.elapsed().as_secs_f64() * 1e3);
                    self.free_slots.push(slot);
                }
                None => self.unmatched += 1,
            }
        }
        Ok(())
    }
}

/// Waits for each submitted conversion's fence value and records submit -> done in ms.
fn watch_fence(fence: ID3D12Fence, rx: mpsc::Receiver<(u64, Instant)>) -> Samples {
    let mut samples = Samples::default();
    // SAFETY: plain call; auto-reset, initially unsignalled, unnamed event.
    let Ok(event) = (unsafe { CreateEventW(None, false, false, None) }) else {
        return samples;
    };
    for (value, submitted) in rx {
        // SAFETY: `event` is a live event handle owned by this thread.
        let reached = unsafe {
            fence.GetCompletedValue() >= value
                || (fence.SetEventOnCompletion(value, event).is_ok()
                    && WaitForSingleObject(event, 2000) == WAIT_OBJECT_0)
        };
        // A timeout (2 s) is left out of the samples; the count shows it.
        if reached {
            samples.push(submitted.elapsed().as_secs_f64() * 1e3);
        }
    }
    // SAFETY: we own the event and close it once.
    let _ = unsafe { CloseHandle(event) };
    samples
}

/// User + kernel CPU time of this process, in seconds.
fn process_cpu_time() -> f64 {
    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    // SAFETY: the pseudo-handle needs no closing; all out-pointers point to valid locals.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    }
    .is_ok();
    let secs = |t: FILETIME| {
        ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64 / 1e7
    };
    if ok {
        secs(kernel) + secs(user)
    } else {
        f64::NAN
    }
}
