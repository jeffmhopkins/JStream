//! File › Video settings…: the collection's base resolution (the canvas every scene is
//! laid out on) and its base frame rate, as OBS's Video settings set them, and the space
//! its blend modes mix in (docs/UI-FEATURES.md#header).
//!
//! All three are collection state, so the dialog reads them from `state.get` and
//! applies what changed as one batch: one undo step, `canvas.set_size`,
//! `canvas.set_frame_rate` (ADR-0119) and `canvas.set_blending` (ADR-0126). A new size
//! also rescales every scene by default ([`rescale`]): the engine moves nothing when the
//! canvas changes (ADR-0022 decision 1), so the same batch carries the placement and
//! camera edits that keep each scene looking as it did. Encoders scale after this picture: an output's
//! video mix has its own resolution and divides the base rate. A program stage that is
//! running keeps the rate it opened with, so the dialog says the new rate applies once
//! every output has stopped.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#header

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::store::Studio;
use jstream_control::protocol::{
    Batch, Blending, Call, Canvas, DEFAULT_BASE_FPS, Edit, FAST_BASE_FPS, Finite32, ItemGeometry,
    ItemInfo, ItemTransform, MAX_FOV_DEG, MIN_FOV_DEG, PlacementForm, SceneCamera, SceneId,
    SceneInfo, Size, SourceInfo, SourceKind, Vec2, Vec3,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

/// The largest canvas side the engine takes, in pixels.
pub const MAX_SIDE: u32 = 8192;

/// The base resolutions offered first: landscape HD to UHD, then vertical.
pub const PRESETS: [(u32, u32); 6] = [
    (1280, 720),
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (1080, 1920),
    (1080, 1080),
];

/// The rates the engine runs at (ADR-0119).
pub const RATES: [u32; 2] = [DEFAULT_BASE_FPS, FAST_BASE_FPS];

/// A side typed into the dialog, or why it can't be used.
pub fn side(text: &str) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(value) if (1..=MAX_SIDE).contains(&value) => Ok(value),
        _ => Err(format!("Enter a whole number from 1 to {MAX_SIDE}.")),
    }
}

/// What the dialog sets: the base resolution, the base rate and the blending space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Video {
    /// The canvas.
    pub canvas: Canvas,
    /// Frames per second.
    pub fps: u32,
    /// The space blend modes mix in.
    pub blending: Blending,
}

impl Default for Video {
    fn default() -> Self {
        Self {
            canvas: Canvas::DEFAULT,
            fps: DEFAULT_BASE_FPS,
            blending: Blending::default(),
        }
    }
}

/// How each blending space reads in the picker.
pub fn blending_label(blending: Blending) -> &'static str {
    match blending {
        Blending::Linear => "Linear light",
        _ => "Browser-style (sRGB)",
    }
}

/// The edits that take the collection from `now` to `wanted`: only what changed, so
/// applying the same values sends nothing.
pub fn edits(now: Video, wanted: Video) -> Vec<Edit> {
    let mut edits = Vec::new();
    if now.canvas != wanted.canvas {
        edits.push(Edit::CanvasSetSize {
            width: wanted.canvas.width,
            height: wanted.canvas.height,
        });
    }
    if now.fps != wanted.fps {
        edits.push(Edit::CanvasSetFrameRate { fps: wanted.fps });
    }
    if now.blending != wanted.blending {
        edits.push(Edit::CanvasSetBlending {
            blending: wanted.blending,
        });
    }
    edits
}

/// How a canvas change maps the old canvas onto the new one: a uniform scale about the
/// top-left corner, then an offset that centres the scaled scene. With the same aspect
/// ratio the offset is zero and every scene fills the new canvas exactly; with another
/// one the scene keeps its proportions and sits in the middle, as a video mix fits the
/// canvas inside its resolution (ADR-0022 decision 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rescale {
    /// The old canvas.
    pub from: Canvas,
    /// The new canvas.
    pub to: Canvas,
}

impl Rescale {
    /// The uniform scale: the larger the old canvas can be drawn inside the new one.
    pub fn scale(self) -> f32 {
        let sx = self.to.width as f32 / self.from.width.max(1) as f32;
        let sy = self.to.height as f32 / self.from.height.max(1) as f32;
        sx.min(sy)
    }

    /// Where the old canvas's top-left corner lands, so the scaled scene is centred.
    fn offset(self) -> (f32, f32) {
        let s = self.scale();
        (
            (self.to.width as f32 - s * self.from.width as f32) * 0.5,
            (self.to.height as f32 - s * self.from.height as f32) * 0.5,
        )
    }

    /// A canvas point's new position.
    fn point(self, x: f32, y: f32) -> (f32, f32) {
        let s = self.scale();
        let (ox, oy) = self.offset();
        (s * x + ox, s * y + oy)
    }

    /// A box's geometry on the new canvas. A `scene` source's native size is the canvas
    /// (ADR-0022), so a box showing one takes the canvas's own change in each direction:
    /// the scene it shows is rescaled too, and its content then lands where it did.
    pub fn geometry(self, geometry: ItemGeometry, shows_canvas: bool) -> ItemGeometry {
        let s = self.scale();
        let (x, y) = self.point(geometry.center.x.get(), geometry.center.y.get());
        let (sw, sh) = if shows_canvas {
            (
                self.to.width as f32 / self.from.width.max(1) as f32,
                self.to.height as f32 / self.from.height.max(1) as f32,
            )
        } else {
            (s, s)
        };
        ItemGeometry {
            center: vec2(x, y, geometry.center),
            size: Size {
                width: finite(geometry.size.width.get() * sw, geometry.size.width),
                height: finite(geometry.size.height.get() * sh, geometry.size.height),
            },
            depth_px: finite(geometry.depth_px.get() * s, geometry.depth_px),
            perspective_px: geometry.perspective_px.map(|p| finite(p.get() * s, p)),
            ..geometry
        }
    }

    /// A legacy placement on the new canvas: its corner moves and, for a source of its
    /// own size, its scale grows with the canvas. A `scene` source grows by itself, since
    /// its native size is the canvas, so only its corner moves, by what keeps the shown
    /// scene's content where it was.
    pub fn transform(self, transform: ItemTransform, shows_canvas: bool) -> ItemTransform {
        let s = self.scale();
        let (mut x, mut y) = self.point(transform.position.x.get(), transform.position.y.get());
        if shows_canvas {
            let (ox, oy) = self.offset();
            let (kx, ky) = (transform.scale.x.get() * ox, transform.scale.y.get() * oy);
            let (sin, cos) = transform.rotation_deg.get().to_radians().sin_cos();
            x -= cos * kx - sin * ky;
            y -= sin * kx + cos * ky;
            ItemTransform {
                position: vec2(x, y, transform.position),
                ..transform
            }
        } else {
            ItemTransform {
                position: vec2(x, y, transform.position),
                scale: vec2(
                    transform.scale.x.get() * s,
                    transform.scale.y.get() * s,
                    transform.scale,
                ),
                ..transform
            }
        }
    }

    /// A scene camera on the new canvas: its position and target move as points, with
    /// depth scaled as the items' is, and its focus and blur scale too. The picture of a
    /// camera spans the canvas's height, so when the scale follows the width instead
    /// (a taller canvas), the field of view widens by what keeps the scene at that scale.
    pub fn camera(self, camera: SceneCamera) -> SceneCamera {
        let s = self.scale();
        let at = |v: Vec3| {
            let (x, y) = self.point(v.x.get(), v.y.get());
            Vec3 {
                x: finite(x, v.x),
                y: finite(y, v.y),
                z: finite(v.z.get() * s, v.z),
            }
        };
        let height = self.to.height as f32 / (s * self.from.height.max(1) as f32);
        let half = (camera.fov_deg.get().to_radians() * 0.5).tan() * height;
        let fov = (half.atan() * 2.0)
            .to_degrees()
            .clamp(MIN_FOV_DEG, MAX_FOV_DEG);
        SceneCamera {
            position: at(camera.position),
            target: at(camera.target),
            fov_deg: finite(fov, camera.fov_deg),
            focus_px: camera.focus_px.map(|f| finite(f.get() * s, f)),
            blur_px: finite(camera.blur_px.get() * s, camera.blur_px),
            ..camera
        }
    }
}

fn finite(value: f32, otherwise: Finite32) -> Finite32 {
    Finite32::new(value).unwrap_or(otherwise)
}

fn vec2(x: f32, y: f32, otherwise: Vec2) -> Vec2 {
    Vec2::new(x, y).unwrap_or(otherwise)
}

/// The edits that keep every scene looking as it did when the canvas goes from `from`
/// to `to`, to send in the batch with `canvas.set_size`: each item's placement in every
/// scene and group, each keyframe scene's overridden placement, and each scene camera
/// with each keyframe's camera override. Crops are in source pixels, so they stay.
/// Nothing when the size doesn't change.
///
/// `scenes` is `state.get`'s, a keyframe scene's `overrides` among them; a keyframe
/// scene's `items` are ignored, since it has none of its own.
pub fn rescale(
    scenes: &[SceneInfo],
    sources: &[SourceInfo],
    from: Canvas,
    to: Canvas,
) -> Vec<Edit> {
    if from == to {
        return Vec::new();
    }
    let map = Rescale { from, to };
    let owners: Vec<&ItemInfo> = scenes
        .iter()
        .filter(|scene| scene.variant_of.is_none())
        .flat_map(|scene| &scene.items)
        .collect();
    let shows_canvas = |item: &ItemInfo| {
        sources
            .iter()
            .find(|source| source.source_id == item.source_id)
            .is_some_and(|source| matches!(source.kind, SourceKind::Scene { .. }))
    };
    let placement = |item: &ItemInfo,
                     scene_id: Option<&SceneId>,
                     geometry: Option<ItemGeometry>,
                     transform: Option<ItemTransform>| {
        let canvas = shows_canvas(item);
        let scene_id = scene_id.cloned();
        match (geometry, transform) {
            (Some(geometry), _) => Some(Edit::ItemSetGeometry {
                item_id: item.item_id.clone(),
                scene_id,
                geometry: map.geometry(geometry, canvas),
            }),
            (None, Some(transform)) => Some(Edit::ItemSetTransform {
                item_id: item.item_id.clone(),
                scene_id,
                transform: map.transform(transform, canvas),
            }),
            (None, None) => None,
        }
    };
    let mut edits = Vec::new();
    for scene in scenes {
        if scene.variant_of.is_none() {
            for item in &scene.items {
                let (geometry, transform) = match item.placement {
                    PlacementForm::Geometry => (item.geometry, None),
                    _ => (None, Some(item.transform)),
                };
                edits.extend(placement(item, None, geometry, transform));
            }
        } else {
            for over in &scene.overrides {
                let Some(item) = owners.iter().find(|item| item.item_id == over.item_id) else {
                    continue;
                };
                edits.extend(placement(
                    item,
                    Some(&scene.scene_id),
                    over.geometry,
                    over.transform,
                ));
            }
        }
        if let Some(camera) = scene.camera {
            edits.push(Edit::SceneSetCamera {
                scene_id: scene.scene_id.clone(),
                camera: Some(map.camera(camera)),
            });
        }
    }
    edits
}

/// How a preset or a size reads in the picker: `1920 × 1080 (16:9)`.
pub fn label(width: u32, height: u32) -> String {
    let (mut a, mut b) = (width, height);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let gcd = a.max(1);
    format!("{width} × {height} ({}:{})", width / gcd, height / gcd)
}

/// The dialog. It reads the collection as it opens and applies on Apply.
#[component]
pub fn VideoSettingsDialog(
    /// Closes the dialog.
    close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let session = expect_context::<SessionContext>();
    let studio = expect_context::<Studio>();
    let read = move || {
        studio.collection.with_untracked(|c| {
            c.as_ref().map_or_else(Video::default, |c| Video {
                canvas: c.canvas,
                fps: c.base_fps,
                blending: c.blending,
            })
        })
    };
    let now = read();
    let width = RwSignal::new(now.canvas.width.to_string());
    let height = RwSignal::new(now.canvas.height.to_string());
    let rate = RwSignal::new(now.fps);
    let blending = RwSignal::new(now.blending);
    let scale_scenes = RwSignal::new(true);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let preset = move || {
        let (w, h) = (side(&width.get()), side(&height.get()));
        PRESETS
            .iter()
            .position(|p| Ok(p.0) == w && Ok(p.1) == h)
            .map_or_else(|| "custom".to_owned(), |i| i.to_string())
    };
    let apply = move || {
        let (w, h) = match (side(&width.get_untracked()), side(&height.get_untracked())) {
            (Ok(w), Ok(h)) => (w, h),
            (Err(why), _) | (_, Err(why)) => {
                error.set(Some(why));
                return;
            }
        };
        let wanted = Video {
            canvas: Canvas {
                width: w,
                height: h,
            },
            fps: rate.get_untracked(),
            blending: blending.get_untracked(),
        };
        let now = read();
        let mut edits = edits(now, wanted);
        if scale_scenes.get_untracked() {
            studio.collection.with_untracked(|c| {
                if let Some(c) = c {
                    edits.extend(rescale(&c.scenes, &c.sources, now.canvas, wanted.canvas));
                }
            });
        }
        if edits.is_empty() {
            close();
            return;
        }
        let Some(session) = session.get() else {
            error.set(Some("Not connected to an engine.".into()));
            return;
        };
        busy.set(true);
        spawn_local(async move {
            let call = Call::Batch(Batch {
                edits,
                expected_rev: None,
                idempotency_key: None,
                ends_gesture: None,
            });
            let sent = session.control.call(call).await;
            busy.set(false);
            match sent {
                Ok(_) => close(),
                Err(err) => error.set(Some(err.to_string())),
            }
        });
    };
    view! {
        <Modal label="Video settings" class="settings-dialog video-settings" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Video settings"</h2>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>"×"</button>
            </div>
            <div class="settings-body">
                <label class="settings-field">
                    <span class="settings-check-label">"Base (canvas) resolution"</span>
                    <select
                        aria-label="Base resolution"
                        prop:value=preset
                        on:change=move |event| {
                            if let Some(&(w, h)) = event_target_value(&event)
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| PRESETS.get(i))
                            {
                                width.set(w.to_string());
                                height.set(h.to_string());
                                error.set(None);
                            }
                        }
                    >
                        {PRESETS.iter().enumerate().map(|(i, &(w, h))| view! {
                            <option value=i.to_string()>{label(w, h)}</option>
                        }).collect_view()}
                        <option value="custom">"Custom"</option>
                    </select>
                </label>
                <div class="settings-size">
                    <input
                        type="number"
                        min="1"
                        max=MAX_SIDE.to_string()
                        aria-label="Width"
                        prop:value=move || width.get()
                        on:input=move |event| {
                            width.set(event_target_value(&event));
                            error.set(None);
                        }
                    />
                    <span>"×"</span>
                    <input
                        type="number"
                        min="1"
                        max=MAX_SIDE.to_string()
                        aria-label="Height"
                        prop:value=move || height.get()
                        on:input=move |event| {
                            height.set(event_target_value(&event));
                            error.set(None);
                        }
                    />
                </div>
                <p class="settings-check-note">
                    "Every scene is laid out on this canvas, and the program and preview show it at this size. Each output's video mix scales from it."
                </p>
                <label class="settings-check">
                    <input
                        type="checkbox"
                        aria-label="Scale scenes to the new resolution"
                        prop:checked=move || scale_scenes.get()
                        on:change=move |event| scale_scenes.set(event_target_checked(&event))
                    />
                    <span>
                        <span class="settings-check-label">"Scale scenes to the new resolution"</span>
                        <span class="settings-check-note">
                            "Every item and scene camera grows or shrinks with the canvas, so each scene looks as it did. A canvas of another shape keeps the scenes' proportions and centres them. Off, items keep their pixel positions. Either way the change is one undo step."
                        </span>
                    </span>
                </label>
                <label class="settings-field">
                    <span class="settings-check-label">"Frame rate"</span>
                    <select
                        aria-label="Frame rate"
                        prop:value=move || rate.get().to_string()
                        on:change=move |event| {
                            if let Ok(fps) = event_target_value(&event).parse::<u32>() {
                                rate.set(fps);
                            }
                        }
                    >
                        {RATES.iter().map(|fps| view! {
                            <option value=fps.to_string()>{format!("{fps} fps")}</option>
                        }).collect_view()}
                    </select>
                </label>
                <p class="settings-check-note">
                    "Outputs run at this rate, or at a share of it set by their video mix. 60 fps doubles every mix's rate and the work per second. While an output is live, a new rate applies once every output has stopped."
                </p>
                <label class="settings-field">
                    <span class="settings-check-label">"Blending"</span>
                    <select
                        aria-label="Blending"
                        prop:value=move || blending.get().as_str()
                        on:change=move |event| {
                            if let Some(space) = Blending::from_name(&event_target_value(&event)) {
                                blending.set(space);
                            }
                        }
                    >
                        {Blending::ALL.iter().map(|space| view! {
                            <option value=space.as_str()>{blending_label(*space)}</option>
                        }).collect_view()}
                    </select>
                </label>
                <p class="settings-check-note">
                    "How every blend mode mixes colours, in every scene. Browser-style matches CSS and design tools, so a design made there keeps its look. Linear light mixes light as it adds, for even glows and fades, but makes a faint veil brighter than a browser draws it."
                </p>
                {move || error.get().map(|why| view! { <p class="settings-error" role="alert">{why}</p> })}
            </div>
            <div class="modal-footer">
                <button type="button" on:click=move |_| close()>"Cancel"</button>
                <button
                    type="button"
                    class="primary"
                    data-primary
                    prop:disabled=move || busy.get()
                    on:click=move |_| apply()
                >
                    "Apply"
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::store::Collection;
    use jstream_control::MockEngine;
    use jstream_control::protocol::{
        CallResult, HistoryCall, HistoryOp, Query, SceneRole, Settings,
    };
    use std::pin::pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    #[test]
    fn only_what_changed_is_sent() {
        let now = Video::default();
        let with = |width, height, fps, blending| Video {
            canvas: Canvas { width, height },
            fps,
            blending,
        };
        assert_eq!(edits(now, now), Vec::<Edit>::new());
        assert_eq!(
            edits(now, with(1920, 1080, 60, Blending::Srgb)),
            vec![Edit::CanvasSetFrameRate { fps: 60 }]
        );
        assert_eq!(
            edits(now, with(1280, 720, 30, Blending::Srgb)),
            vec![Edit::CanvasSetSize {
                width: 1280,
                height: 720
            }]
        );
        assert_eq!(
            edits(now, with(1920, 1080, 30, Blending::Linear)),
            vec![Edit::CanvasSetBlending {
                blending: Blending::Linear
            }]
        );
        assert_eq!(
            edits(
                with(1920, 1080, 60, Blending::Linear),
                with(3840, 2160, 30, Blending::Srgb)
            )
            .len(),
            3
        );
    }

    #[test]
    fn sides_are_checked() {
        assert_eq!(side(" 1920 "), Ok(1920));
        assert_eq!(side("8192"), Ok(MAX_SIDE));
        assert!(side("0").is_err());
        assert!(side("8193").is_err());
        assert!(side("19.5").is_err());
        assert!(side("").is_err());
    }

    #[test]
    fn presets_read_with_their_aspect() {
        assert_eq!(label(1920, 1080), "1920 × 1080 (16:9)");
        assert_eq!(label(1080, 1920), "1080 × 1920 (9:16)");
        assert_eq!(label(1080, 1080), "1080 × 1080 (1:1)");
        for (w, h) in PRESETS {
            assert_eq!(side(&w.to_string()), Ok(w));
            assert_eq!(side(&h.to_string()), Ok(h));
        }
        assert!(RATES.contains(&DEFAULT_BASE_FPS));
    }

    fn canvas(width: u32, height: u32) -> Canvas {
        Canvas { width, height }
    }

    fn f(value: f32) -> Finite32 {
        Finite32::new(value).unwrap_or_else(|| panic!("{value} is not finite"))
    }

    fn boxed(x: f32, y: f32, w: f32, h: f32) -> ItemGeometry {
        ItemGeometry {
            center: Vec2 { x: f(x), y: f(y) },
            size: Size {
                width: f(w),
                height: f(h),
            },
            fit_mode: Default::default(),
            rotation_deg: f(30.0),
            tilt_deg: f(10.0),
            turn_deg: Finite32::ZERO,
            depth_px: f(100.0),
            perspective_px: Some(f(2000.0)),
            flip_h: true,
            flip_v: false,
            billboard: false,
            surface: None,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn the_same_shape_scales_every_box_with_the_canvas() {
        let map = Rescale {
            from: canvas(1920, 1080),
            to: canvas(1280, 720),
        };
        let s = 2.0 / 3.0;
        assert!(close(map.scale(), s));
        let g = map.geometry(boxed(960.0, 540.0, 300.0, 150.0), false);
        assert!(close(g.center.x.get(), 640.0) && close(g.center.y.get(), 360.0));
        assert!(close(g.size.width.get(), 200.0) && close(g.size.height.get(), 100.0));
        assert!(close(g.depth_px.get(), 100.0 * s));
        assert!(close(
            g.perspective_px.map_or(0.0, Finite32::get),
            2000.0 * s
        ));
        assert_eq!(
            (g.rotation_deg, g.tilt_deg, g.flip_h),
            (f(30.0), f(10.0), true)
        );
        // A box showing a scene grows as the canvas does, which is the same here.
        assert_eq!(
            map.geometry(boxed(10.0, 20.0, 30.0, 40.0), true),
            map.geometry(boxed(10.0, 20.0, 30.0, 40.0), false)
        );
    }

    #[test]
    fn another_shape_keeps_proportions_and_centres_the_scene() {
        // 16:9 to 1:1: the width limits, so the scene is centred vertically.
        let map = Rescale {
            from: canvas(1920, 1080),
            to: canvas(1080, 1080),
        };
        let s = 1080.0 / 1920.0;
        assert!(close(map.scale(), s));
        let g = map.geometry(boxed(0.0, 0.0, 1920.0, 1080.0), false);
        let top = (1080.0 - 1080.0 * s) / 2.0;
        assert!(close(g.center.x.get(), 0.0) && close(g.center.y.get(), top));
        assert!(close(g.size.width.get(), 1080.0) && close(g.size.height.get(), 1080.0 * s));
        // The canvas centre stays the centre.
        let mid = map.geometry(boxed(960.0, 540.0, 10.0, 10.0), false);
        assert!(close(mid.center.x.get(), 540.0) && close(mid.center.y.get(), 540.0));
        // A full-canvas box showing a scene covers the new canvas, whose own content
        // the same rescale centres, so that content lands where the box's did.
        let nest = map.geometry(boxed(960.0, 540.0, 1920.0, 1080.0), true);
        assert!(close(nest.size.width.get(), 1080.0) && close(nest.size.height.get(), 1080.0));
        assert!(close(nest.center.x.get(), 540.0) && close(nest.center.y.get(), 540.0));
    }

    #[test]
    fn a_legacy_placement_moves_its_corner_and_scales() {
        let map = Rescale {
            from: canvas(1280, 720),
            to: canvas(1920, 1080),
        };
        let t = ItemTransform {
            position: Vec2 {
                x: f(100.0),
                y: f(50.0),
            },
            scale: Vec2 {
                x: f(0.5),
                y: f(2.0),
            },
            rotation_deg: f(15.0),
        };
        let own = map.transform(t, false);
        assert!(close(own.position.x.get(), 150.0) && close(own.position.y.get(), 75.0));
        assert!(close(own.scale.x.get(), 0.75) && close(own.scale.y.get(), 3.0));
        assert_eq!(own.rotation_deg, f(15.0));
        // A scene source grows by itself: only its corner moves.
        let nest = map.transform(t, true);
        assert!(close(nest.position.x.get(), 150.0) && close(nest.position.y.get(), 75.0));
        assert_eq!(nest.scale, t.scale);
    }

    #[test]
    fn a_camera_keeps_its_framing() {
        let same = Rescale {
            from: canvas(1920, 1080),
            to: canvas(3840, 2160),
        };
        let mut framing = SceneCamera::framing(canvas(1920, 1080));
        framing.focus_px = Some(f(500.0));
        framing.blur_px = f(8.0);
        let moved = same.camera(framing);
        let want = SceneCamera::framing(canvas(3840, 2160));
        assert!(close(moved.position.x.get(), want.position.x.get()));
        assert!(close(moved.position.y.get(), want.position.y.get()));
        assert!((moved.position.z.get() - want.position.z.get()).abs() < 0.5);
        assert!(close(moved.fov_deg.get(), want.fov_deg.get()));
        assert_eq!((moved.focus_px, moved.blur_px), (Some(f(1000.0)), f(16.0)));
        // A taller canvas scaled by its width widens the view, so the scene's height
        // spans the same share of the new canvas as its items' boxes do.
        let tall = Rescale {
            from: canvas(1920, 1080),
            to: canvas(1080, 1920),
        };
        let cam = tall.camera(SceneCamera::framing(canvas(1920, 1080)));
        let s = tall.scale();
        let seen = 2.0 * cam.position.z.get() * (cam.fov_deg.get().to_radians() / 2.0).tan();
        assert!((seen - 1920.0).abs() < 0.5, "{seen}");
        assert!(close(cam.target.y.get(), 960.0) && close(cam.target.x.get(), 540.0));
        assert!(close(
            cam.position.z.get(),
            SceneCamera::framing(canvas(1920, 1080)).position.z.get() * s
        ));
    }

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    fn batch(session: &Session, edits: Vec<Edit>) {
        let call = Call::Batch(Batch {
            edits,
            expected_rev: None,
            idempotency_key: None,
            ends_gesture: None,
        });
        now(session.control.call(call)).unwrap_or_else(|e| panic!("batch failed: {e}"));
    }

    fn state(session: &Session) -> Collection {
        let read = now(session.control.call(Call::Query {
            query: Query::StateGet,
        }))
        .unwrap_or_else(|e| panic!("state.get failed: {e}"));
        let CallResult::Query { result } = read.result else {
            panic!("not a query result");
        };
        Collection::from_result(result).unwrap_or_else(|| panic!("not a state.get result"))
    }

    #[test]
    fn a_rescale_rides_the_size_change_as_one_undo_step() {
        let engine = MockEngine::new();
        let session = now(Session::open(Rc::new(engine.connect())))
            .unwrap_or_else(|e| panic!("hello failed: {e}"));
        let add = |scene: &str, item: &str, source: &str, geometry| Edit::ItemAdd {
            scene_id: scene.into(),
            item_id: item.into(),
            source_id: source.into(),
            transform: None,
            geometry: Some(geometry),
            opacity: None,
            blend: None,
        };
        batch(
            &session,
            vec![
                Edit::SceneCreate {
                    scene_id: "main".into(),
                    name: "Main".into(),
                    role: None,
                },
                Edit::SceneCreate {
                    scene_id: "g".into(),
                    name: "Group".into(),
                    role: Some(SceneRole::Group),
                },
                Edit::SourceCreate {
                    source_id: "red".into(),
                    name: "Red".into(),
                    kind: SourceKind::Builtin {
                        kind: "color".into(),
                    },
                    settings: Settings::new(),
                    duplicated_from: None,
                },
                Edit::SourceCreate {
                    source_id: "group".into(),
                    name: "Group".into(),
                    kind: SourceKind::Scene {
                        scene_id: "g".into(),
                    },
                    settings: Settings::new(),
                    duplicated_from: None,
                },
                add("main", "box", "red", boxed(960.0, 540.0, 400.0, 200.0)),
                add("main", "nest", "group", boxed(960.0, 540.0, 1920.0, 1080.0)),
                add("g", "child", "red", boxed(100.0, 100.0, 50.0, 50.0)),
                Edit::SceneCreateVariant {
                    scene_id: "wide".into(),
                    variant_of: "main".into(),
                    name: "Wide".into(),
                },
                Edit::ItemSetGeometry {
                    item_id: "box".into(),
                    scene_id: Some("wide".into()),
                    geometry: boxed(480.0, 270.0, 800.0, 400.0),
                },
                Edit::SceneSetCamera {
                    scene_id: "main".into(),
                    camera: Some(SceneCamera::framing(canvas(1920, 1080))),
                },
            ],
        );
        let before = state(&session);
        let to = canvas(1280, 720);
        let mut edits = edits(
            Video {
                canvas: before.canvas,
                ..Video::default()
            },
            Video {
                canvas: to,
                ..Video::default()
            },
        );
        edits.extend(rescale(&before.scenes, &before.sources, before.canvas, to));
        batch(&session, edits);

        let after = state(&session);
        assert_eq!(after.canvas, to);
        let geometry = |c: &Collection, scene: &str, item: &str| {
            c.scene(&SceneId::new(scene))
                .and_then(|s| s.items.iter().find(|i| i.item_id.as_str() == item))
                .and_then(|i| i.geometry)
                .unwrap_or_else(|| panic!("no {item} in {scene}"))
        };
        assert!(close(
            geometry(&after, "main", "box").size.width.get(),
            400.0 * 2.0 / 3.0
        ));
        assert_eq!(geometry(&after, "main", "nest").size.width, f(1280.0));
        assert!(close(
            geometry(&after, "g", "child").center.x.get(),
            100.0 * 2.0 / 3.0
        ));
        assert_eq!(geometry(&after, "wide", "box").center.x, f(320.0));
        let camera = after
            .scene(&SceneId::new("main"))
            .and_then(|s| s.camera)
            .unwrap_or_else(|| panic!("no camera"));
        assert!(close(camera.target.x.get(), 640.0));

        let undo = Call::History(HistoryCall::new(HistoryOp::Undo));
        now(session.control.call(undo)).unwrap_or_else(|e| panic!("undo failed: {e}"));
        let undone = state(&session);
        assert_eq!(undone.canvas, before.canvas);
        assert_eq!(
            undone.scenes, before.scenes,
            "one undo restores every scene"
        );
    }

    #[test]
    fn an_unchanged_size_rescales_nothing() {
        assert_eq!(
            rescale(&[], &[], Canvas::DEFAULT, Canvas::DEFAULT),
            Vec::new()
        );
    }
}
