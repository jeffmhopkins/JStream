//! The `backdrop` source: a generated studio backdrop, like the painted canvas, muslin or
//! seamless paper a photographer hangs behind a subject.
//!
//! It is drawn once, on the CPU, on create and on each update. The look comes from
//! what makes a photographed backdrop read as real rather than computer-made: a mottle
//! of two related colours from domain-warped fractal noise (cloud shapes with no
//! repeating pattern), a soft pool of light where the key light falls, falloff toward
//! the edges, a fine fibre texture, and grain over everything, which also hides banding
//! in the smooth gradients. Colours mix and light in linear light, then are encoded to
//! sRGB. The same settings always draw the same picture: `seed` picks another one.

use crate::color::{frame_size, parse_color, rgba_frame};
use crate::describe::{kind, param, size};
use crate::source::{changed_non_live, setting};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{EnumOption, KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;
use std::f32::consts::FRAC_1_SQRT_2;
use std::sync::Arc;

/// The `backdrop` kind.
pub(crate) struct BackdropKind(KindDescriptor);

/// The styles, by option value, with their label.
const STYLES: [(&str, &str); 3] = [
    ("canvas", "Painted canvas"),
    ("muslin", "Muslin"),
    ("paper", "Seamless paper"),
];

/// The largest `seed`.
const MAX_SEED: i64 = 999_999;

impl BackdropKind {
    pub(crate) fn new() -> Self {
        let live = |p: ParamDescriptor| ParamDescriptor { live: true, ..p };
        let colour = || ParamType::Color { alpha: false };
        let amount = || ParamType::Float {
            min: Some(0.0),
            max: Some(1.0),
            step: Some(0.01),
            unit: None,
        };
        let described = |p: ParamDescriptor, text: &str| ParamDescriptor {
            description: Some(text.into()),
            ..p
        };
        Self(KindDescriptor {
            params: vec![
                live(described(
                    param(
                        "style",
                        "Style",
                        ParamType::Enum {
                            options: STYLES
                                .iter()
                                .map(|(value, label)| EnumOption {
                                    value: (*value).into(),
                                    label: (*label).into(),
                                })
                                .collect(),
                        },
                        json!("canvas"),
                    ),
                    "Painted canvas has brushy, broken mottling; muslin softer, larger clouds; \
                     seamless paper is a smooth sweep with almost none.",
                )),
                live(described(
                    param("color", "Color", colour(), json!("#56697f")),
                    "The backdrop's main color, where the light falls.",
                )),
                live(described(
                    param("accent", "Accent", colour(), json!("#202c3a")),
                    "The second color mottled in. A darker or shifted shade of Color \
                     looks most like paint.",
                )),
                live(described(
                    param("texture", "Texture", amount(), json!(0.6)),
                    "How strongly the two colors mottle. 0 is a plain gradient.",
                )),
                live(described(
                    param(
                        "scale",
                        "Scale",
                        ParamType::Float {
                            min: Some(0.25),
                            max: Some(4.0),
                            step: Some(0.05),
                            unit: None,
                        },
                        json!(1.0),
                    ),
                    "The size of the mottling: above 1 is larger clouds.",
                )),
                live(described(
                    param("light", "Light", amount(), json!(0.6)),
                    "How bright the pool of light is.",
                )),
                live(described(
                    param("light_x", "Light X", amount(), json!(0.5)),
                    "Where the light falls, from the left edge (0) to the right (1).",
                )),
                live(described(
                    param("light_y", "Light Y", amount(), json!(0.42)),
                    "Where the light falls, from the top (0) to the bottom (1).",
                )),
                live(described(
                    param("vignette", "Vignette", amount(), json!(0.5)),
                    "How much the edges and corners fall off into shadow.",
                )),
                live(described(
                    param("grain", "Grain", amount(), json!(0.35)),
                    "Film grain over everything.",
                )),
                live(described(
                    param(
                        "seed",
                        "Seed",
                        ParamType::Int {
                            min: Some(0),
                            max: Some(MAX_SEED),
                            step: Some(1),
                            unit: None,
                        },
                        json!(1),
                    ),
                    "Another number draws another backdrop with the same look.",
                )),
                param("width", "Width", size(), json!(1920)),
                param("height", "Height", size(), json!(1080)),
            ],
            ..kind("backdrop", "Backdrop", true, false)
        })
    }
}

impl SourceKind for BackdropKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let size = frame_size(settings, &ctx.limits)?;
        let look = Look::read(settings)?;
        Ok(Box::new(BackdropSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            frame: Arc::new(paint(size, &look)?),
        }))
    }
}

/// A running `backdrop` source. Its one frame is drawn on create and on each update.
struct BackdropSource {
    descriptor: KindDescriptor,
    settings: Settings,
    frame: Arc<VideoFrame>,
}

impl Source for BackdropSource {
    fn frame(&mut self, _: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(Arc::clone(&self.frame))
    }

    fn next_change(&self, _: MediaTime) -> Option<MediaTime> {
        None
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let look = Look::read(settings)?;
        self.frame = Arc::new(paint(self.frame.info().size, &look)?);
        self.settings = settings.clone();
        Ok(())
    }
}

/// How a backdrop's mottle is built, per style.
#[derive(Clone, Copy, Debug)]
struct Style {
    /// Cloud cells across the frame's width at a `scale` of 1.
    cells: f32,
    /// How far the noise bends itself: more is more painterly swirl.
    warp: f32,
    octaves: u32,
    /// Each octave's weight against the one before: more is rougher.
    roughness: f32,
    /// How much of `texture` the style uses.
    strength: f32,
    /// How strong the fine fibre texture is.
    fibre: f32,
    /// How much smaller, sharper blotches break up the clouds, as brush or sponge dabs.
    dabs: f32,
}

impl Style {
    fn named(name: &str) -> Option<Style> {
        Some(match name {
            "canvas" => Style {
                cells: 2.6,
                warp: 1.0,
                octaves: 6,
                roughness: 0.58,
                strength: 1.0,
                fibre: 0.7,
                dabs: 0.45,
            },
            "muslin" => Style {
                cells: 1.9,
                warp: 2.2,
                octaves: 5,
                roughness: 0.5,
                strength: 0.9,
                fibre: 0.45,
                dabs: 0.15,
            },
            "paper" => Style {
                cells: 1.2,
                warp: 0.6,
                octaves: 3,
                roughness: 0.4,
                strength: 0.18,
                fibre: 0.15,
                dabs: 0.0,
            },
            _ => return None,
        })
    }
}

/// Every setting but the size, read and checked.
#[derive(Clone, Copy, Debug)]
struct Look {
    style: Style,
    /// Linear-light RGB.
    color: [f32; 3],
    accent: [f32; 3],
    texture: f32,
    scale: f32,
    light: f32,
    light_at: (f32, f32),
    vignette: f32,
    grain: f32,
    seed: u32,
}

impl Look {
    fn read(settings: &Settings) -> Result<Look, SourceError> {
        let style = setting(settings, "style")?
            .as_str()
            .and_then(Style::named)
            .ok_or_else(|| bad("style", "must be `canvas`, `muslin` or `paper`"))?;
        let seed = setting(settings, "seed")?
            .as_i64()
            .filter(|s| (0..=MAX_SEED).contains(s))
            .ok_or_else(|| {
                bad(
                    "seed",
                    &format!("must be a whole number from 0 to {MAX_SEED}"),
                )
            })?;
        Ok(Look {
            style,
            color: linear(colour(settings, "color")?),
            accent: linear(colour(settings, "accent")?),
            texture: amount(settings, "texture", 0.0, 1.0)?,
            scale: amount(settings, "scale", 0.25, 4.0)?,
            light: amount(settings, "light", 0.0, 1.0)?,
            light_at: (
                amount(settings, "light_x", 0.0, 1.0)?,
                amount(settings, "light_y", 0.0, 1.0)?,
            ),
            vignette: amount(settings, "vignette", 0.0, 1.0)?,
            grain: amount(settings, "grain", 0.0, 1.0)?,
            // At most 999 999, which a u32 holds.
            seed: seed as u32,
        })
    }
}

fn bad(param: &str, reason: &str) -> SourceError {
    SourceError::Setting {
        param: param.to_owned(),
        reason: reason.to_owned(),
    }
}

fn colour(settings: &Settings, id: &str) -> Result<[u8; 4], SourceError> {
    setting(settings, id)?
        .as_str()
        .and_then(parse_color)
        .ok_or_else(|| bad(id, "must be a color as `#rrggbb` or `#rrggbbaa`"))
}

fn amount(settings: &Settings, id: &str, min: f64, max: f64) -> Result<f32, SourceError> {
    setting(settings, id)?
        .as_f64()
        .filter(|v| (min..=max).contains(v))
        // Within ±4, which an f32 holds closely enough.
        .map(|v| v as f32)
        .ok_or_else(|| bad(id, &format!("must be a number from {min} to {max}")))
}

/// How many frame pixels each way one cell of the mottle grid covers. The mottle is
/// smooth at that size, so it is worked out on a grid this much coarser and
/// interpolated, which keeps a 4K backdrop quick to draw.
const COARSE: usize = 4;

/// Draws a backdrop of `size` with `look`: opaque RGBA in sRGB.
fn paint(size: FrameSize, look: &Look) -> Result<VideoFrame, SourceError> {
    let (w, h) = (size.width() as usize, size.height() as usize);
    let noise = Noise::new(look.seed);
    let style = look.style;
    // Noise coordinates: the frame's width spans `cells` cells, square ones.
    let cells = style.cells / look.scale;
    let unit = cells / w as f32;

    let aspect = w as f32 / h.max(1) as f32;
    // The pool of light: a wide Gaussian, in units of the frame's height.
    let pool = 0.5;
    let (lx, ly) = (look.light_at.0 * aspect, look.light_at.1);
    // The mottle and the light on the coarse grid, one sample past each edge for
    // interpolation. Both are smooth at that size.
    let (gw, gh) = (w / COARSE + 2, h / COARSE + 2);
    let mut mottle = vec![0f32; gw * gh];
    let mut light = vec![0f32; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let (px, py) = ((gx * COARSE) as f32, (gy * COARSE) as f32);
            // The mottle, -1 to 1 or so, sharpened a little so the clouds have edges.
            let m = noise.painted(px * unit, py * unit, &style) * 1.8;
            mottle[gy * gw + gx] = m / (1.0 + m.abs() * 0.6);
            // The pool where the key light falls, and the falloff to the edges.
            let (u, v) = ((px + 0.5) / h as f32, (py + 0.5) / h as f32);
            let (dx, dy) = (u - lx, v - ly);
            let lit = (-(dx * dx + dy * dy) / (2.0 * pool * pool)).exp();
            let (cx, cy) = ((u / aspect - 0.5) * 2.0, (v - 0.5) * 2.0);
            let edge = smoothstep(0.25, 1.5, (cx * cx * 0.7 + cy * cy).sqrt());
            light[gy * gw + gx] =
                (0.5 + 1.25 * look.light * lit) * (1.0 - 0.85 * look.vignette * edge);
        }
    }
    let sample = |grid: &[f32], x: usize, y: usize| {
        let (x0, y0) = (x / COARSE, y / COARSE);
        let (tx, ty) = (
            (x % COARSE) as f32 / COARSE as f32,
            (y % COARSE) as f32 / COARSE as f32,
        );
        let at = |x: usize, y: usize| grid[y.min(gh - 1) * gw + x.min(gw - 1)];
        let top = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * tx;
        let bottom = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * tx;
        top + (bottom - top) * ty
    };

    let texture = look.texture * style.strength;
    let srgb = Encoder::new();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let m = sample(&mottle, x, y);
            // Fine fibres: a stretched, high-frequency noise, as woven or brushed.
            let fibre = noise.gradient_noise(x as f32 * 0.21, y as f32 * 0.13 + 31.0, 9)
                * style.fibre
                * look.texture;
            let t = (0.5 + 0.5 * m * texture).clamp(0.0, 1.0);
            let mut c = [0f32; 3];
            for (i, channel) in c.iter_mut().enumerate() {
                *channel = look.accent[i] + (look.color[i] - look.accent[i]) * t;
            }
            let gain = sample(&light, x, y) * (1.0 + 0.4 * m * texture + 0.1 * fibre);
            // Grain, as film's: brighter areas take less of it.
            let g = (hash(x as u32, y as u32, look.seed) - 0.5) * 2.0;
            let grain = g * (0.004 + 0.05 * look.grain);
            for channel in &mut c {
                let encoded = srgb.encode(*channel * gain);
                *channel = encoded + grain * (1.0 - encoded * 0.5);
            }
            // Dithered to 8 bits, so smooth sweeps don't band.
            let d = hash(x as u32 ^ 0x9e37, y as u32, look.seed ^ 0x5bd1) - 0.5;
            rgba.extend_from_slice(&[
                byte(c[0] * 255.0 + d),
                byte(c[1] * 255.0 + d),
                byte(c[2] * 255.0 + d),
                255,
            ]);
        }
    }
    Ok(rgba_frame(size, rgba, MediaTime::zero(Timebase::MILLIS))?)
}

fn byte(v: f32) -> u8 {
    // Clamped to 0..=255 first, so the cast is exact.
    v.round().clamp(0.0, 255.0) as u8
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// An sRGB byte as linear light.
fn decode(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light as an sRGB value from 0 to 1.
fn encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// [`encode`] from a table, which a frame's millions of samples need: linear light in
/// 4096 steps, finer than 8-bit sRGB's darkest step.
struct Encoder(Vec<f32>);

impl Encoder {
    const STEPS: usize = 4096;

    fn new() -> Self {
        Self(
            (0..=Self::STEPS)
                .map(|i| encode(i as f32 / Self::STEPS as f32))
                .collect(),
        )
    }

    /// Linear light, clamped to 0 to 1, as sRGB, interpolated between steps.
    fn encode(&self, c: f32) -> f32 {
        let at = c.clamp(0.0, 1.0) * Self::STEPS as f32;
        // From 0 to STEPS, so both indices are in the table.
        let i = (at as usize).min(Self::STEPS - 1);
        let t = at - i as f32;
        self.0[i] + (self.0[i + 1] - self.0[i]) * t
    }
}

fn linear(rgba: [u8; 4]) -> [f32; 3] {
    [decode(rgba[0]), decode(rgba[1]), decode(rgba[2])]
}

/// A number from 0 to 1 for a pixel and a seed: a fixed integer hash, so a backdrop is
/// the same on every machine.
fn hash(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x
        .wrapping_mul(0x8da6_b343)
        .wrapping_add(y.wrapping_mul(0xd816_3841))
        .wrapping_add(seed.wrapping_mul(0xcb1a_b31f));
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Sixteen unit vectors evenly around the circle, the gradients noise picks from.
const GRADIENTS: [(f32, f32); 16] = [
    (1.0, 0.0),
    (0.923_88, 0.382_68),
    (FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    (0.382_68, 0.923_88),
    (0.0, 1.0),
    (-0.382_68, 0.923_88),
    (-FRAC_1_SQRT_2, FRAC_1_SQRT_2),
    (-0.923_88, 0.382_68),
    (-1.0, 0.0),
    (-0.923_88, -0.382_68),
    (-FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
    (-0.382_68, -0.923_88),
    (0.0, -1.0),
    (0.382_68, -0.923_88),
    (FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
    (0.923_88, -0.382_68),
];

/// Seeded 2D gradient noise, and fractal sums of it.
struct Noise {
    seed: u32,
}

impl Noise {
    fn new(seed: u32) -> Self {
        Self { seed }
    }

    /// The gradient at a lattice point: one of [`GRADIENTS`], picked by a hash.
    fn gradient(&self, ix: i32, iy: i32, octave: u32) -> (f32, f32) {
        // Lattice coordinates as their two's-complement bits, so negatives hash too.
        let h = hash(ix as u32, iy as u32, self.seed.wrapping_add(octave * 7919));
        // From 0 to 1, so the index is below 16.
        GRADIENTS[(h * 16.0) as usize & 15]
    }

    /// Gradient noise at (x, y), about -0.7 to 0.7, smooth with a quintic fade.
    fn gradient_noise(&self, x: f32, y: f32, octave: u32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        // Within the frame's few hundred cells, which an i32 holds.
        let (ix, iy) = (x0 as i32, y0 as i32);
        let dot = |cx: i32, cy: i32, dx: f32, dy: f32| {
            let (gx, gy) = self.gradient(ix + cx, iy + cy, octave);
            gx * dx + gy * dy
        };
        let fade = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let (u, v) = (fade(fx), fade(fy));
        let a = dot(0, 0, fx, fy);
        let b = dot(1, 0, fx - 1.0, fy);
        let c = dot(0, 1, fx, fy - 1.0);
        let d = dot(1, 1, fx - 1.0, fy - 1.0);
        let top = a + (b - a) * u;
        let bottom = c + (d - c) * u;
        top + (bottom - top) * v
    }

    /// `octaves` of gradient noise, each twice the frequency and `roughness` times the
    /// weight of the one before, normalised to about -1 to 1.
    fn fbm(&self, x: f32, y: f32, octaves: u32, roughness: f32) -> f32 {
        let (mut sum, mut weight, mut total, mut f) = (0.0, 1.0, 0.0, 1.0);
        for octave in 0..octaves {
            // Each octave turned a little, so their grids don't line up.
            let (rx, ry) = rotate(x * f, y * f, octave);
            sum += weight * self.gradient_noise(rx, ry, octave);
            total += weight;
            weight *= roughness;
            f *= 2.0;
        }
        sum / total * 1.4
    }

    /// The painted mottle: fractal noise looked up through two more fractal noises
    /// (domain warping), which bends the clouds into brushy, uneven shapes.
    fn painted(&self, x: f32, y: f32, style: &Style) -> f32 {
        let (o, r) = (style.octaves, style.roughness);
        let qx = self.fbm(x + 1.7, y + 9.2, o.min(4), r);
        let qy = self.fbm(x + 8.3, y + 2.8, o.min(4), r);
        let clouds = self.fbm(x + style.warp * qx, y + style.warp * qy, o, r);
        if style.dabs > 0.0 {
            let dabs = self.fbm(x * 3.1 + 40.0 + qy, y * 3.1 - 17.0 + qx, 3, 0.5);
            clouds + style.dabs * dabs
        } else {
            clouds
        }
    }
}

/// The sine and cosine of each octave's turn: 0.5 + 1.1 × octave radians.
const TURNS: [(f32, f32); 8] = [
    (0.479426, 0.877583),
    (0.999574, -0.029200),
    (0.427380, -0.904072),
    (-0.611858, -0.790968),
    (-0.982453, 0.186512),
    (-0.279415, 0.960170),
    (0.728969, 0.684547),
    (0.940731, -0.339155),
];

/// (x, y) turned by a fixed angle per octave, and shifted, so octaves don't align.
fn rotate(x: f32, y: f32, octave: u32) -> (f32, f32) {
    // At most 6 octaves are summed, within the table.
    let (s, c) = TURNS[octave as usize % TURNS.len()];
    (
        x * c - y * s + octave as f32 * 13.7,
        x * s + y * c + octave as f32 * 5.3,
    )
}

#[cfg(test)]
mod tests {
    use super::{Style, smoothstep};

    #[test]
    fn styles_are_named() {
        assert!(Style::named("canvas").is_some());
        assert!(Style::named("muslin").is_some());
        assert!(Style::named("paper").is_some());
        assert!(Style::named("velvet").is_none());
    }

    /// The table holds the turns `rotate` documents.
    #[test]
    fn the_turns_are_their_angles() {
        for (octave, &(s, c)) in super::TURNS.iter().enumerate() {
            let (es, ec) = (0.5 + octave as f32 * 1.1).sin_cos();
            assert!(
                (s - es).abs() < 1e-5 && (c - ec).abs() < 1e-5,
                "octave {octave}"
            );
        }
    }

    #[test]
    fn smoothstep_clamps() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
    }

    /// [`hash`](super::hash) puts about a tenth of 100 000 samples in each tenth of 0
    /// to 1.
    #[test]
    fn the_hash_is_spread_evenly() {
        let mut bins = [0u32; 10];
        for i in 0..100_000u32 {
            let v = super::hash(i % 317, i / 317, 7);
            bins[(v * 10.0) as usize] += 1;
        }
        assert!(
            bins.iter().all(|&n| (9_000..11_000).contains(&n)),
            "{bins:?}"
        );
    }
}
