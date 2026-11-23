//! The `fog_card` source: a generated card of fog, one colour whose opacity is cloud
//! shapes that thin out raggedly toward the card's edges, so it never reads as a
//! rectangle.
//!
//! It stands in for fog marched through a scene's depth (`volumetric_fog`), which costs
//! a march per pixel per frame: a card costs what any still picture does. Placed as an
//! item in a 3D scene, in front of what it should veil, it is a plane at a depth, so it
//! shifts against what is behind it as the camera moves, and a few at different depths
//! read as fog with volume.
//!
//! It is drawn once, on the CPU, on create and on each update, from the same seeded
//! noise as a `backdrop`, so the same settings always draw the same card.

use crate::backdrop::{Noise, amount, bad, byte, colour, hash, smoothstep};
use crate::color::{frame_size, rgba_frame};
use crate::describe::{kind, param};
use crate::source::{changed_non_live, setting};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, Timebase, VideoFrame};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use serde_json::json;
use std::sync::Arc;

/// The `fog_card` kind.
pub(crate) struct FogCardKind(KindDescriptor);

/// The largest `seed`.
const MAX_SEED: i64 = 999_999;

/// The longest side a card may have. It is a soft picture that scales up with no
/// visible loss, and it is drawn on the CPU when it is created or changed.
const MAX_SIDE: u32 = 4096;

/// Cloud cells across the card's shorter side at a `scale` of 1.
const CELLS: f32 = 1.8;

/// How far the noise bends itself, which makes the clouds wispy.
const WARP: f32 = 0.9;

/// How much of the noise's range the fog takes to go from clear to its most.
const SOFTNESS: f32 = 0.5;

/// How many pixels each way one cell of the grid the clouds are worked out on covers.
const COARSE: usize = 4;

impl FogCardKind {
    pub(crate) fn new() -> Self {
        let amount = || ParamType::Float {
            min: Some(0.0),
            max: Some(1.0),
            step: Some(0.01),
            unit: None,
        };
        let live = |p: ParamDescriptor, text: &str| ParamDescriptor {
            live: true,
            description: Some(text.into()),
            ..p
        };
        let side = || ParamType::Int {
            min: Some(1),
            max: Some(i64::from(MAX_SIDE)),
            step: Some(1),
            unit: Some("px".into()),
        };
        let sized = |p: ParamDescriptor| ParamDescriptor {
            description: Some("At most 4096. A card is soft and scales up smoothly.".into()),
            ..p
        };
        Self(KindDescriptor {
            params: vec![
                live(
                    param(
                        "color",
                        "Color",
                        ParamType::Color { alpha: false },
                        json!("#c8ccd2"),
                    ),
                    "The fog's color. Place the card in a 3D scene in front of what it \
                     should veil: a few cards at different depths read as fog with volume.",
                ),
                live(
                    param("density", "Density", amount(), json!(0.5)),
                    "How opaque the thickest fog is.",
                ),
                live(
                    param("coverage", "Coverage", amount(), json!(0.8)),
                    "How much of the card is fog: 0 is clear, 1 fog nearly everywhere.",
                ),
                live(
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
                    "The size of the clouds: above 1 is larger.",
                ),
                live(
                    param("edge", "Edge fade", amount(), json!(0.7)),
                    "How far in from the edges the fog thins out, breaking up into wisps \
                     so the card has no outline. 0 keeps fog up to the edges.",
                ),
                live(
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
                    "Another number draws other clouds with the same look.",
                ),
                sized(param("width", "Width", side(), json!(1024))),
                sized(param("height", "Height", side(), json!(576))),
            ],
            ..kind("fog_card", "Fog card", true, false)
        })
    }
}

impl SourceKind for FogCardKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let size = frame_size(settings, &ctx.limits)?;
        for (id, side) in [("width", size.width()), ("height", size.height())] {
            if side > MAX_SIDE {
                return Err(bad(
                    id,
                    &format!("must be a whole number from 1 to {MAX_SIDE}"),
                ));
            }
        }
        let look = Look::read(settings)?;
        Ok(Box::new(FogCardSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            frame: Arc::new(paint(size, &look)?),
        }))
    }
}

/// A running `fog_card` source. Its one frame is drawn on create and on each update.
struct FogCardSource {
    descriptor: KindDescriptor,
    settings: Settings,
    frame: Arc<VideoFrame>,
}

impl Source for FogCardSource {
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

/// Every setting but the size, read and checked.
#[derive(Clone, Copy, Debug)]
struct Look {
    /// Straight sRGB bytes.
    color: [u8; 3],
    density: f32,
    coverage: f32,
    scale: f32,
    edge: f32,
    seed: u32,
}

impl Look {
    fn read(settings: &Settings) -> Result<Look, SourceError> {
        let seed = setting(settings, "seed")?
            .as_i64()
            .filter(|s| (0..=MAX_SEED).contains(s))
            .ok_or_else(|| {
                bad(
                    "seed",
                    &format!("must be a whole number from 0 to {MAX_SEED}"),
                )
            })?;
        let [r, g, b, _] = colour(settings, "color")?;
        Ok(Look {
            color: [r, g, b],
            density: amount(settings, "density", 0.0, 1.0)?,
            coverage: amount(settings, "coverage", 0.0, 1.0)?,
            scale: amount(settings, "scale", 0.25, 4.0)?,
            edge: amount(settings, "edge", 0.0, 1.0)?,
            // At most 999 999, which a u32 holds.
            seed: seed as u32,
        })
    }
}

/// How much fog there is, 0 to 1, where the clouds' noise is `cloud` (0 to 1) and the
/// card is `inside` (0 at an edge that fades, 1 where no edge is near).
///
/// The noise must pass a threshold that `coverage` lowers and nearness to an edge
/// raises, up to above anything the noise reaches at the edge itself. So toward an edge
/// only the densest clouds remain, then none: the fog ends in wisps, not along a line.
fn fog(cloud: f32, inside: f32, coverage: f32) -> f32 {
    let threshold = 1.0 - coverage * inside;
    smoothstep(threshold, threshold + SOFTNESS, cloud)
}

/// Draws a card of `size` with `look`: straight-alpha RGBA in sRGB, the colour
/// everywhere and the fog in its alpha.
fn paint(size: FrameSize, look: &Look) -> Result<VideoFrame, SourceError> {
    let (w, h) = (size.width() as usize, size.height() as usize);
    let noise = Noise::new(look.seed);
    let unit = CELLS / look.scale / w.min(h) as f32;
    // How far the pixel at (x, y) is inside the card: 1 at the centre, falling over the
    // outer `edge` of the way out to 0 on a rounded outline through the middle of each
    // side, so the corners are clear and no side shows as a straight line.
    let inside = |x: usize, y: usize| {
        if look.edge <= 0.0 {
            return 1.0;
        }
        let u = ((x as f32 + 0.5) / w as f32 * 2.0 - 1.0).abs();
        let v = ((y as f32 + 0.5) / h as f32 * 2.0 - 1.0).abs();
        let out = (u * u * u + v * v * v).cbrt();
        ((1.0 - out) / look.edge).clamp(0.0, 1.0)
    };
    // The clouds on a coarse grid, one sample past each edge for interpolation: they
    // are smooth at that size.
    let (gw, gh) = (w / COARSE + 2, h / COARSE + 2);
    let mut clouds = vec![0f32; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let (x, y) = ((gx * COARSE) as f32 * unit, (gy * COARSE) as f32 * unit);
            // Fractal noise looked up through two more (domain warping), for wisps.
            let qx = noise.fbm(x + 1.7, y + 9.2, 3, 0.5);
            let qy = noise.fbm(x + 8.3, y + 2.8, 3, 0.5);
            let n = noise.fbm(x + WARP * qx, y + WARP * qy, 5, 0.5);
            clouds[gy * gw + gx] = (0.5 + 0.8 * n).clamp(0.0, 1.0);
        }
    }
    let sample = |x: usize, y: usize| {
        let (x0, y0) = (x / COARSE, y / COARSE);
        let (tx, ty) = (
            (x % COARSE) as f32 / COARSE as f32,
            (y % COARSE) as f32 / COARSE as f32,
        );
        let at = |x: usize, y: usize| clouds[y.min(gh - 1) * gw + x.min(gw - 1)];
        let top = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * tx;
        let bottom = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * tx;
        top + (bottom - top) * ty
    };
    let [r, g, b] = look.color;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let amount = fog(sample(x, y), inside(x, y), look.coverage);
            // Dithered to 8 bits, so thin fog doesn't band; clear stays clear.
            let alpha = amount * look.density * 255.0;
            let d = hash(x as u32, y as u32, look.seed ^ 0x5bd1) - 0.5;
            let alpha = if alpha > 0.0 { byte(alpha + d) } else { 0 };
            rgba.extend_from_slice(&[r, g, b, alpha]);
        }
    }
    Ok(rgba_frame(size, rgba, MediaTime::zero(Timebase::MILLIS))?)
}

#[cfg(test)]
mod tests {
    use super::fog;

    #[test]
    fn fog_needs_denser_cloud_toward_an_edge_and_ends_at_it() {
        // At an edge that fades, not even the densest cloud is fog.
        assert_eq!(fog(1.0, 0.0, 1.0), 0.0);
        // Well inside, more coverage makes fog of thinner cloud.
        assert_eq!(fog(0.6, 1.0, 0.0), 0.0);
        assert!(fog(0.6, 1.0, 0.6) > 0.0);
        assert!(fog(0.6, 1.0, 1.0) > fog(0.6, 1.0, 0.6));
        // The same cloud is less fog nearer the edge.
        assert!(fog(0.8, 0.4, 0.6) < fog(0.8, 0.9, 0.6));
        // The densest cloud well inside is all fog at any coverage that has some.
        assert!((fog(1.0, 1.0, 0.6) - 1.0).abs() < 1e-6);
    }
}
