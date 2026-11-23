//! Reflection probes (ADR-0170 decisions 2 and 3): each `reflection_probe` item a scene
//! places captures the scene from its box's centre into a panorama, and a `scene`-lit
//! model or lit card inside the box reflects that capture in place of the environment.
//!
//! Only a scene whose `reflections` option is on captures or reads its probes, and only
//! while the `reflection_probes` switch is on. A capture runs after the frame's chains,
//! so it sees every item's layer as this frame drew it, and a surface reads the last
//! capture: a probe shows in the frame after it first captures. A capture draws the
//! scene six times, through 90-degree cameras on square faces at the probe's resolution
//! (capped by the switch's `max_probe_resolution`), shaded by its lights and
//! environment only: no probe, no shadows, no filters, so a capture never reads itself.
//! A capture is spread over frames: it draws one face a frame and resamples the six
//! into the probe's panorama in the frame it draws the last, so a refresh costs a sixth
//! of a capture each frame rather than all of it in one. Until then surfaces read the
//! probe's previous capture. One probe captures at a time: the due one that has waited
//! longest since it last tried, so a scene with many spreads their cost and none is
//! starved.
//!
//! A probe captures when it is first drawn, when its box or settings change, and every
//! `refresh_frames` after that; a probe that captures once also captures again
//! [`SETTLE_FRAMES`] after its first, when what it sees has loaded.

use jstream_media::{FrameSize, MediaTime};
use jstream_protocol::{Canvas, Finite32, ItemGeometry, ItemId, SceneCamera, SceneId, Vec3};
use jstream_render::{Color, Compositor, Item, Placement, RenderError, ResourceCache};
use jstream_sources::ShownProbe;

use super::{
    Captures, GpuKey, LiveParams, Shot, fill_items, groups, motion, pass_models, pose, scene,
    scene_environment, scene_lights, scene_pass,
};
use crate::chain::Chains;

/// How many frames after its first capture a probe that captures once captures again.
pub(crate) const SETTLE_FRAMES: u64 = 60;

/// A probe's box in its scene now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ProbeBox {
    /// Its centre, in the scene's space.
    centre: [f32; 3],
    /// Half its size along the scene's axes.
    half: [f32; 3],
    /// How far inside its faces a surface takes all of it.
    fade: f32,
}

impl ProbeBox {
    fn contains(&self, point: [f64; 3]) -> bool {
        (0..3).all(|i| (point[i] - f64::from(self.centre[i])).abs() <= f64::from(self.half[i]))
    }

    fn volume(&self) -> f64 {
        self.half.iter().map(|h| f64::from(*h)).product()
    }
}

/// What a probe was last captured with, kept per compositor.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProbeState {
    bounds: ProbeBox,
    /// The face size it was captured at.
    face: u32,
    refresh: u32,
    /// The frame of the cache it last captured in, and whether that was its settling
    /// capture or a later one.
    captured: u64,
    settled: bool,
}

/// A capture under way: which probe, what it is captured with, and how far it got.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProbeRun {
    key: (SceneId, ItemId),
    bounds: ProbeBox,
    face: u32,
    refresh: u32,
    settled: bool,
    /// The faces drawn so far.
    drawn: usize,
    /// Each drawn face's camera axes.
    axes: [[[f32; 3]; 3]; 6],
}

/// The probes scene `id` places now, in stacking order: each item with its box and
/// settings. A probe whose item is hidden, or isn't placed in the scene's space, has
/// none.
fn boxes<'s>(
    shot: &'s Shot,
    id: &SceneId,
    at: MediaTime,
    live: &LiveParams,
) -> Vec<(&'s ItemId, ProbeBox, &'s ShownProbe)> {
    let Some(placed) = shot.probes.get(id) else {
        return Vec::new();
    };
    placed
        .iter()
        .filter_map(|placed| {
            let probe = shot.frames.get(&placed.source)?.probe.as_ref()?;
            let (placement, crop) = live.apply_to(
                &placed.item,
                placed.through.as_ref(),
                placed.placement,
                placed.crop,
            );
            let (item, modifiers) = &placed.motion.to;
            let by = shot.motion_at(item, modifiers, at);
            let (placement, _, opacity) = motion::apply(&by, placement, crop, placed.opacity);
            let (Placement::Geometry(geometry), opacity) =
                groups::place_placement(&placed.groups, (placement, opacity), shot, at, live)?
            else {
                return None;
            };
            (opacity > 0.0).then(|| {
                let centre = centre_of(geometry).map(|c| c as f32);
                (
                    &placed.item,
                    ProbeBox {
                        centre,
                        half: probe.size_px.map(|size| size * 0.5),
                        fade: probe.fade_px,
                    },
                    probe,
                )
            })
        })
        .collect()
}

/// The centre of `geometry`'s box in the scene's space.
pub(super) fn centre_of(geometry: ItemGeometry) -> [f64; 3] {
    let corners = geometry.corners_in_scene();
    [0, 1, 2].map(|i| corners.iter().map(|corner| corner[i]).sum::<f64>() / 4.0)
}

/// The probe a surface at `point` in scene `id` reflects (ADR-0170 decision 3): the
/// smallest box holding it among the probes captured at least once, or `None` outside
/// every box, in a scene whose `reflections` is off, or with the switch off.
pub(super) fn lookup<'c>(
    (cache, chains): (&'c ResourceCache<GpuKey>, &Chains),
    shot: &Shot,
    id: &SceneId,
    point: [f64; 3],
    (at, live): (MediaTime, &LiveParams),
) -> Option<jstream_render::ProbeLookup<'c>> {
    if !shot.features.reflection_probes || !shot.reflective.contains(id) {
        return None;
    }
    boxes(shot, id, at, live)
        .into_iter()
        .filter(|(_, bounds, _)| bounds.contains(point))
        .filter_map(|(item, bounds, _)| {
            let state = chains.probes.get(&(id.clone(), item.clone()))?;
            Some((item, bounds, state))
        })
        .min_by(|a, b| a.1.volume().total_cmp(&b.1.volume()))
        .and_then(|(item, _, state)| {
            // The box it was captured from, which the panorama's projection needs.
            let captured = state.bounds;
            Some(jstream_render::ProbeLookup {
                image: cache.get(&GpuKey::ProbeMap(id.clone(), item.clone()))?,
                centre: captured.centre,
                half: captured.half,
                fade: captured.fade,
            })
        })
}

/// The six cameras a probe at `centre` captures through, looking along +x, -x, +y, -y,
/// +z and -z, each a 90-degree view on a square face. `None` for a centre out of range.
fn face_cameras(centre: [f32; 3], base: SceneCamera) -> Option<[SceneCamera; 6]> {
    const AIMS: [[f32; 3]; 6] = [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
    ];
    let vec3 = |v: [f32; 3]| {
        Some(Vec3 {
            x: Finite32::new(v[0])?,
            y: Finite32::new(v[1])?,
            z: Finite32::new(v[2])?,
        })
    };
    let position = vec3(centre)?;
    let mut cameras = [base; 6];
    for (camera, aim) in cameras.iter_mut().zip(AIMS) {
        *camera = SceneCamera {
            position,
            target: vec3([0, 1, 2].map(|i| centre[i] + aim[i] * 1000.0))?,
            fov_deg: Finite32::new(90.0)?,
            roll_deg: Finite32::ZERO,
            focus_px: None,
            blur_px: Finite32::ZERO,
            ..base
        };
    }
    Some(cameras)
}

/// Captures the probes due this frame (see the module docs), at most one, and keeps
/// every other probe's last capture for the surfaces that read it.
///
/// # Errors
/// What the compositor reports for making or drawing into a capture's layers.
pub(super) fn draw_probes(
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
    (shot, at): (&Shot, MediaTime),
    (live, captures): (&LiveParams, &dyn Captures),
) -> Result<(), RenderError> {
    // A probe whose capture the cache let go captures again.
    chains.probes.retain(|(scene, item), _| {
        cache
            .get(&GpuKey::ProbeMap(scene.clone(), item.clone()))
            .is_some()
    });
    if !shot.features.reflection_probes {
        chains.probes.clear();
        chains.probe_tries.clear();
        chains.probe_run = None;
        return Ok(());
    }
    let frame = cache.frame();
    let mut due = Vec::new();
    for scene_id in &shot.reflective {
        for (item, bounds, probe) in boxes(shot, scene_id, at, live) {
            let face = shot.features.probe_face(probe.resolution);
            let key = (scene_id.clone(), item.clone());
            let map = GpuKey::ProbeMap(scene_id.clone(), item.clone());
            let state = chains.probes.get(&key);
            // The last capture is kept for the surfaces that read it until the next
            // replaces it.
            if let Some(state) = state {
                cache.layer(compositor, &map, state.face * 4, state.face * 2)?;
            }
            let kept = match state {
                Some(state)
                    if state.bounds == bounds
                        && state.face == face
                        && state.refresh == probe.refresh_frames =>
                {
                    let refresh = match state.refresh {
                        0 if !state.settled => SETTLE_FRAMES,
                        0 => u64::MAX,
                        frames => u64::from(frames),
                    };
                    frame.saturating_sub(state.captured) < refresh
                }
                _ => false,
            };
            if !kept {
                due.push((scene_id, item, bounds, probe, face));
            }
        }
    }
    // Forget the tries of probes no longer drawn or due.
    chains.probe_tries.retain(|key, _| {
        chains.probes.contains_key(key)
            || due
                .iter()
                .any(|(scene_id, item, ..)| key.0 == **scene_id && key.1 == **item)
    });
    // The due probe that has waited longest goes first: one never tried before any
    // other, then the one whose last try is oldest, so a probe due every frame can't
    // keep the frame's one capture from the rest.
    due.sort_by_key(|(scene_id, item, ..)| {
        chains
            .probe_tries
            .get(&((*scene_id).clone(), (*item).clone()))
            .map_or(0, |frame| frame + 1)
    });
    // The capture under way goes on while its probe is still due as it started; one
    // whose probe went or changed starts again.
    let run = chains.probe_run.take().filter(|run| {
        due.iter().any(|(scene_id, item, bounds, probe, face)| {
            run.key.0 == **scene_id
                && run.key.1 == **item
                && run.bounds == *bounds
                && run.face == *face
                && run.refresh == probe.refresh_frames
        })
    });
    let runs = run.into_iter().chain(due.into_iter().map(
        |(scene_id, item, bounds, probe, face)| {
            let key = (scene_id.clone(), item.clone());
            let settled = chains
                .probes
                .get(&key)
                .is_some_and(|state| state.bounds == bounds && state.face == face);
            ProbeRun {
                key,
                bounds,
                face,
                refresh: probe.refresh_frames,
                settled,
                drawn: 0,
                axes: [[[0.0; 3]; 3]; 6],
            }
        },
    ));
    for mut run in runs.collect::<Vec<_>>() {
        // A try counts whether or not it draws, so one that can't keeps no slot.
        chains.probe_tries.insert(run.key.clone(), frame);
        if !capture(
            compositor,
            (cache, chains),
            (shot, at),
            &mut run,
            (live, captures),
        )? {
            continue;
        }
        if run.drawn < 6 {
            chains.probe_run = Some(run);
        } else {
            chains.probes.insert(
                run.key,
                ProbeState {
                    bounds: run.bounds,
                    face: run.face,
                    refresh: run.refresh,
                    captured: frame,
                    settled: run.settled,
                },
            );
        }
        break;
    }
    Ok(())
}

/// Draws `run`'s next face: its scene from the probe's centre through that face's
/// camera, and once the sixth is drawn resamples the six into the probe's
/// [`GpuKey::ProbeMap`]. Returns `false` when there was nothing to draw: no pass draws
/// the scene, or it has no camera.
fn capture(
    compositor: &mut Compositor,
    (cache, chains): (&mut ResourceCache<GpuKey>, &mut Chains),
    (shot, at): (&Shot, MediaTime),
    run: &mut ProbeRun,
    (live, captures): (&LiveParams, &dyn Captures),
) -> Result<bool, RenderError> {
    let (scene_id, item) = (&run.key.0, &run.key.1);
    let (bounds, face) = (run.bounds, run.face);
    let Some(pass) = scene_pass(shot, scene_id) else {
        return Ok(false);
    };
    let canvas = shot.canvas()?;
    let face_size = FrameSize::new(face, face).map_err(|_| RenderError::Invalid {
        what: "probe face",
        detail: "a probe face has no pixels".into(),
    })?;
    let face_canvas = Canvas {
        width: face,
        height: face,
    };
    let size = [canvas.width() as f32, canvas.height() as f32];
    let Some(main) = super::moved_camera(
        live.camera(pass),
        pass.camera.as_ref(),
        (shot, at, shot.pass_shown(pass)),
        (live, captures),
        chains,
    ) else {
        return Ok(false);
    };
    let Some(cameras) = face_cameras(bounds.centre, main) else {
        return Ok(false);
    };
    // Every layer first, while the cache can change, the faces drawn in earlier frames
    // kept with them.
    let map = GpuKey::ProbeMap(scene_id.clone(), item.clone());
    cache.layer(compositor, &map, face * 4, face * 2)?;
    let faces: Vec<GpuKey> = (0..6)
        .map(|i| GpuKey::ProbeFace(scene_id.clone(), i))
        .collect();
    for key in &faces {
        cache.layer(compositor, key, face, face)?;
    }
    let models = pass_models((&*cache, &*chains), shot, pass, (at, size), live, main);
    for shown in &models {
        let key = GpuKey::ProbeModel(shown.scene.clone(), shown.item.clone());
        cache.layer(compositor, &key, face, face)?;
    }
    let cache = &*cache;
    let mut items = Vec::new();
    if fill_items(
        &mut items,
        (cache, &*chains),
        (shot, pass),
        (at, canvas),
        (live, captures),
    )
    .is_none()
    {
        return Ok(false);
    }
    let environment = scene_environment((shot, cache), scene_id, at, live).0;
    let mut lights = Vec::new();
    let mut shadowed = Vec::new();
    let mut seen: Vec<Item<'_>> = Vec::with_capacity(items.len());
    let i = run.drawn;
    if let Some(camera) = cameras.get(i) {
        let view = camera
            .view(face_canvas)
            .map_err(|error| RenderError::Invalid {
                what: "probe face",
                detail: error.to_string(),
            })?;
        run.axes[i] = view.axes().map(|axis| axis.map(|c| c as f32));
        lights.clear();
        shadowed.clear();
        scene_lights(
            shot,
            scene_id,
            (at, &view),
            live,
            (&mut lights, &mut shadowed),
        );
        for shown in &models {
            let Some(target) =
                cache.get(&GpuKey::ProbeModel(shown.scene.clone(), shown.item.clone()))
            else {
                continue;
            };
            pose(compositor, shown.model, shown.shown);
            let (drives, driving) = crate::sourced::materials(
                shot.source_drives(shown.item),
                |m| shot.source_value(shown.item, m, at),
                |m| shot.tint(shown.item, m),
            );
            let (texture, reflection) = super::model_maps(shot, cache, shown.source, shown.shown);
            let lit = shown.shown.lighting == jstream_media::model::Lighting::Scene;
            compositor.draw_model(
                &jstream_render::ModelDraw {
                    model: shown.model,
                    geometry: shown.geometry,
                    camera: Some(*camera),
                    lighting: shown.shown.lighting,
                    lights: lit.then_some(lights.as_slice()),
                    shadows: &[],
                    environment: if lit { environment } else { None },
                    floor: None,
                    materials: &drives[..driving],
                    texture,
                    reflection,
                    // A capture never reads a probe, so it never reads itself.
                    probe: None,
                },
                target,
            )?;
        }
        seen.clear();
        for item in &items {
            // An item without a camera isn't in the scene's space.
            if item.camera.is_none() {
                continue;
            }
            let image = models
                .iter()
                .find(|shown| {
                    cache
                        .get(shown.layer)
                        .is_some_and(|image| image.texture() == item.image.texture())
                })
                .and_then(|shown| {
                    cache.get(&GpuKey::ProbeModel(shown.scene.clone(), shown.item.clone()))
                })
                .unwrap_or(item.image);
            seen.push(Item {
                image,
                camera: Some(*camera),
                // A model's depth is drawn through the scene's camera, not this face's.
                depth: None,
                model: None,
                ..*item
            });
        }
        let Some(target) = cache.get(&faces[i]) else {
            return Ok(false);
        };
        compositor.render_into(&scene(face_size, Color::TRANSPARENT, &seen), target)?;
        run.drawn += 1;
    }
    if run.drawn < 6 {
        return Ok(true);
    }
    let mut drawn = Vec::with_capacity(6);
    for (key, axes) in faces.iter().zip(run.axes) {
        let Some(image) = cache.get(key) else {
            return Ok(false);
        };
        drawn.push(jstream_render::ProbeFace { image, axes });
    }
    let Ok(drawn) = <[jstream_render::ProbeFace<'_>; 6]>::try_from(drawn) else {
        return Ok(false);
    };
    let Some(target) = cache.get(&map) else {
        return Ok(false);
    };
    compositor.resample_probe(&drawn, target)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{ProbeBox, face_cameras};
    use jstream_protocol::{Canvas, SceneCamera};

    #[test]
    fn a_box_holds_what_is_inside_it_and_the_smaller_wins() {
        let big = ProbeBox {
            centre: [0.0; 3],
            half: [100.0; 3],
            fade: 0.0,
        };
        let small = ProbeBox {
            half: [10.0; 3],
            ..big
        };
        assert!(big.contains([99.0, -99.0, 0.0]));
        assert!(!big.contains([101.0, 0.0, 0.0]));
        assert!(small.volume() < big.volume());
    }

    /// Each face looks along its axis, 90 degrees across, so the six cover every
    /// direction once.
    #[test]
    fn the_six_faces_look_along_the_axes() {
        let base = SceneCamera::framing(Canvas {
            width: 1280,
            height: 720,
        });
        let cameras = face_cameras([10.0, 20.0, 30.0], base).expect("in range");
        let square = Canvas {
            width: 64,
            height: 64,
        };
        let aims = [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        for (camera, aim) in cameras.iter().zip(aims) {
            let view = camera.view(square).expect("a view");
            let [right, down, forward] = view.axes();
            for i in 0..3 {
                assert!(
                    (forward[i] - aim[i]).abs() < 1e-4,
                    "{forward:?} along {aim:?}"
                );
            }
            let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            assert!(dot(right, forward).abs() < 1e-4 && dot(down, forward).abs() < 1e-4);
            assert!(
                (f64::from(view.focal_px()) - 32.0).abs() < 0.01,
                "90 degrees"
            );
        }
    }
}
