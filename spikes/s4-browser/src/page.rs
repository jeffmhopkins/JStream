//! The built-in test page and the pixel layout the checks rely on.
//!
//! Row [`PROBE_Y`] of every frame holds, from the left:
//! - a 16-bit frame counter, one 16 px cell per bit (least significant first), white for 1
//!   and black for 0, both opaque. The page bumps it on every `requestAnimationFrame`, so
//!   consecutive paints show whether a page frame was skipped or painted twice;
//! - an opaque red cell;
//! - a green cell at 50 % opacity;
//! - a cell the page never draws, which must stay fully transparent.
//!
//! Below that band a box moves across the page, so every frame changes. The page also
//! plays two WebAudio tones, [`TONE_LEFT_HZ`] on the left channel and [`TONE_RIGHT_HZ`] on
//! the right, and logs one `S4 {json}` console line with its user agent, the codecs it
//! can play and the audio context's state.

pub const PROBE_Y: u32 = 40;
pub const BIT_CELL: u32 = 16;
pub const COUNTER_BITS: u32 = 16;
pub const RED_X: u32 = 256;
pub const GREEN_X: u32 = 384;
pub const CLEAR_X: u32 = 512;
pub const CELL: u32 = 128;
/// Pixels read back from the probe row: the counter and the three cells.
pub const ROW_PIXELS: u32 = 640;
/// Height of the probe band; the moving box stays below it.
pub const BAND_H: u32 = 80;

pub const TONE_LEFT_HZ: f64 = 440.0;
pub const TONE_RIGHT_HZ: f64 = 660.0;
/// Linear amplitude of each tone (-12 dBFS).
pub const TONE_GAIN: f64 = 0.25;

/// Prefix of the console line the page reports through.
pub const REPORT_PREFIX: &str = "S4 ";

/// The test page for a `width` x `height` view.
pub fn html(width: u32, height: u32) -> String {
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>S4 test page</title>
<style>html,body{{margin:0;padding:0;background:transparent;overflow:hidden}}
canvas{{position:absolute;left:0;top:0}}</style></head>
<body><canvas id="c" width="{width}" height="{height}"></canvas>
<script>
const W = {width}, H = {height}, BAND = {band}, BIT = {bit}, BITS = {bits};
const ctx = document.getElementById('c').getContext('2d');
let n = 0;
function frame() {{
  ctx.clearRect(0, 0, W, H);
  for (let i = 0; i < BITS; i++) {{
    ctx.fillStyle = ((n >> i) & 1) ? '#ffffff' : '#000000';
    ctx.fillRect(i * BIT, 0, BIT, BAND);
  }}
  ctx.fillStyle = 'rgba(255,0,0,1)';
  ctx.fillRect({red_x}, 0, {cell}, BAND);
  ctx.fillStyle = 'rgba(0,255,0,0.5)';
  ctx.fillRect({green_x}, 0, {cell}, BAND);
  // A moving, colour-cycling box below the probe band, so every frame differs.
  const size = Math.min(240, H - BAND - 20);
  const x = (n * 8) % Math.max(1, W - size);
  ctx.fillStyle = `hsl(${{(n * 3) % 360}}, 80%, 55%)`;
  ctx.fillRect(x, BAND + 20, size, size);
  ctx.fillStyle = 'rgba(255,255,255,0.8)';
  ctx.font = '48px sans-serif';
  ctx.fillText('S4 frame ' + n, 40, Math.min(H - 20, BAND + size + 90));
  n = (n + 1) & 0xffff;
  requestAnimationFrame(frame);
}}
requestAnimationFrame(frame);

const report = {{ ua: navigator.userAgent, codecs: {{}}, mse: {{}}, audio: 'not started' }};
const types = {{
  h264: 'video/mp4; codecs="avc1.42E01E"',
  aac: 'audio/mp4; codecs="mp4a.40.2"',
  hevc: 'video/mp4; codecs="hev1.1.6.L93.B0"',
  vp8: 'video/webm; codecs="vp8"',
  vp9: 'video/webm; codecs="vp9"',
  av1: 'video/mp4; codecs="av01.0.05M.08"',
  opus: 'audio/webm; codecs="opus"',
  vorbis: 'audio/ogg; codecs="vorbis"',
  flac: 'audio/flac',
  mp3: 'audio/mpeg',
  wav: 'audio/wav; codecs="1"',
}};
const probe = document.createElement('video');
for (const [k, t] of Object.entries(types)) {{
  report.codecs[k] = probe.canPlayType(t);
  report.mse[k] = (window.MediaSource && MediaSource.isTypeSupported(t)) || false;
}}
try {{
  const ac = new AudioContext({{ sampleRate: 48000 }});
  const merge = ac.createChannelMerger(2);
  for (const [hz, ch] of [[{left}, 0], [{right}, 1]]) {{
    const osc = ac.createOscillator();
    const gain = ac.createGain();
    osc.frequency.value = hz;
    gain.gain.value = {gain};
    osc.connect(gain).connect(merge, 0, ch);
    osc.start();
  }}
  merge.connect(ac.destination);
  ac.resume().then(() => {{ report.audio = ac.state; }}, (e) => {{ report.audio = 'resume failed: ' + e; }});
  report.audio = ac.state;
  report.audio_sample_rate = ac.sampleRate;
}} catch (e) {{
  report.audio = 'error: ' + e;
}}
const send = () => console.log('{prefix}' + JSON.stringify(report));
send();
setTimeout(send, 1000);
</script></body></html>
"#,
        band = BAND_H,
        bit = BIT_CELL,
        bits = COUNTER_BITS,
        red_x = RED_X,
        green_x = GREEN_X,
        cell = CELL,
        left = TONE_LEFT_HZ,
        right = TONE_RIGHT_HZ,
        gain = TONE_GAIN,
        prefix = REPORT_PREFIX,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_fits_the_probe_row() {
        assert_eq!(BIT_CELL * COUNTER_BITS, RED_X);
        assert_eq!(RED_X + CELL, GREEN_X);
        assert_eq!(GREEN_X + CELL, CLEAR_X);
        assert_eq!(CLEAR_X + CELL, ROW_PIXELS);
        const { assert!(PROBE_Y < BAND_H) };
        // Rows are read back with a 256-byte-aligned stride.
        assert_eq!(ROW_PIXELS * 4 % 256, 0);
    }

    #[test]
    fn page_uses_the_layout() {
        let page = html(1920, 1080);
        assert!(page.contains(r#"width="1920" height="1080""#));
        assert!(page.contains("fillRect(256, 0, 128, BAND)"));
        assert!(page.contains("console.log('S4 '"));
        assert!(!page.contains("{{"), "format escapes leaked into the page");
    }
}
