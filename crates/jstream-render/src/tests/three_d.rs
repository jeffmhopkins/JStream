//! Goldens for ADR-0050's 3D drawing, spike S5's scenarios A, B, D and E
//! (`spikes/s5-3d`), at a quarter of its size.
//!
//! Each scene is drawn on the GPU and by a CPU reference that averages N×N point samples
//! per pixel, each mapped back into every item's plane with
//! [`jstream_protocol::ItemGeometry::unplace`] and sampled bilinearly from the same
//! texels at full resolution, so it is the box-filtered image mips approximate. The errors
//! are per channel, in sRGB 8-bit steps, over the pixels the scenario is about. Scenario
//! C, minification, draws a 512² source; the rest a 64² one.

use std::io::Write;

use jstream_media::FrameSize;
use jstream_protocol::{
    CameraView, CanvasPoint, ContentRect, Crop as ProtocolCrop, Finite32, FitMode, ItemGeometry,
    NEAR_W, Placed, SceneCamera, Size, Vec2, Vec3,
};

use super::{item, open};
use crate::color::srgb_channel_to_linear;
use crate::{Color, Crop, Image, Placement, Scene, Snapshot};

pub(super) const WIDTH: u32 = 240;
pub(super) const HEIGHT: u32 = 136;
const SOURCE: u32 = 64;
/// Scenario C's source: big enough that the floor is minified several times over.
pub(super) const LARGE_SOURCE: u32 = 512;
/// The background, dark grey so an item's edge has contrast against it.
const BACKGROUND: [u8; 3] = [20, 20, 20];

/// A `size`² pattern: an 8-texel checkerboard of two colours, with a brighter texel ring
/// every 16, so a bent or shifted texture shows, and a minified one aliases.
pub(super) fn pattern(size: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let pixel = if x % 16 == 0 || y % 16 == 0 {
                [240, 230, 120, 255]
            } else if ((x / 8) + (y / 8)) % 2 == 0 {
                [200, 60, 40, 255]
            } else {
                [40, 120, 220, 255]
            };
            rgba.extend(pixel);
        }
    }
    rgba
}

pub(super) fn geometry(
    center: (f32, f32),
    size: (f32, f32),
    rotation: f32,
    tilt: f32,
    turn: f32,
    depth: f32,
    perspective: Option<f32>,
) -> ItemGeometry {
    let finite = |value: f32| Finite32::new(value).expect("finite");
    ItemGeometry {
        fit_mode: FitMode::Stretch,
        rotation_deg: finite(rotation),
        tilt_deg: finite(tilt),
        turn_deg: finite(turn),
        depth_px: finite(depth),
        perspective_px: perspective.map(finite),
        ..ItemGeometry::try_new(
            Vec2::new(center.0, center.1).expect("finite"),
            Size::try_new(size.0, size.1).expect("positive"),
        )
        .expect("positive")
    }
}

/// The reference image (sRGB) and how much of each pixel the items cover.
pub(super) struct Reference {
    rgb: Vec<[f32; 3]>,
    coverage: Vec<f32>,
}

/// The pattern's texels in linear light.
pub(super) struct Texels {
    size: u32,
    linear: Vec<[f32; 3]>,
}

impl Texels {
    pub(super) fn new(rgba: &[u8], size: u32) -> Self {
        Self {
            size,
            linear: rgba
                .chunks_exact(4)
                .map(|p| [0, 1, 2].map(|c| srgb_channel_to_linear(f32::from(p[c]) / 255.0)))
                .collect(),
        }
    }

    /// Bilinear, clamped to the edge of the crop, with texel centres at
    /// `(i + 0.5) / size`, as the compositor samples a 3D item's chain, which holds only
    /// the cropped texels: nothing past the crop is read, even at its edge.
    fn bilinear(&self, u: f32, v: f32, crop: Crop) -> [f32; 3] {
        let size = self.size as f32;
        let (left, top) = (crop.left as f32, crop.top as f32);
        let right = (self.size - crop.right) as f32 - 1.0;
        let bottom = (self.size - crop.bottom) as f32 - 1.0;
        let x = (u * size - 0.5).clamp(left, right);
        let y = (v * size - 0.5).clamp(top, bottom);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |x: f32, y: f32| {
            let (x, y) = (x.min(right) as usize, y.min(bottom) as usize);
            self.linear[y * self.size as usize + x]
        };
        let (a, b) = (at(x0, y0), at(x0 + 1.0, y0));
        let (c, d) = (at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        [0, 1, 2].map(|i| {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            top + (bottom - top) * fy
        })
    }
}

fn content(geometry: ItemGeometry, size: u32, crop: Crop) -> ContentRect {
    let crop = ProtocolCrop {
        left: crop.left,
        top: crop.top,
        right: crop.right,
        bottom: crop.bottom,
    };
    geometry
        .content(size, size, crop)
        .expect("valid")
        .expect("pixels")
}

/// What `geometry` shows at a canvas point, seen through `view` in a 3D scene, in
/// linear light, or `None` outside it.
fn sample(
    geometry: ItemGeometry,
    view: Option<&CameraView>,
    content: &ContentRect,
    at: CanvasPoint,
    texels: &Texels,
    crop: Crop,
) -> Option<[f32; 3]> {
    let local = geometry.unplace_seen(view, at).expect("valid")?;
    let (half_w, half_h) = (
        geometry.size.width.get() * 0.5,
        geometry.size.height.get() * 0.5,
    );
    let inside_box = local.x.abs() <= half_w && local.y.abs() <= half_h;
    let inside_content = local.x >= content.left
        && local.x <= content.left + content.width
        && local.y >= content.top
        && local.y <= content.top + content.height;
    if !inside_box || !inside_content {
        return None;
    }
    let vertex = geometry
        .drawn_vertex_seen(view, content, local)
        .expect("valid")?;
    debug_assert!(vertex.w >= NEAR_W);
    Some(texels.bilinear(vertex.uv[0], vertex.uv[1], crop))
}

pub(super) fn reference(items: &[ItemGeometry], texels: &Texels, n: u32, crop: Crop) -> Reference {
    reference_seen(items, None, texels, n, crop)
}

/// [`reference`] for a 3D scene with `camera` (ADR-0094): every item seen through it,
/// far to near when it sorts.
fn reference_seen(
    items: &[ItemGeometry],
    camera: Option<SceneCamera>,
    texels: &Texels,
    n: u32,
    crop: Crop,
) -> Reference {
    let view = camera.map(|camera| {
        camera
            .view(jstream_protocol::Canvas {
                width: WIDTH,
                height: HEIGHT,
            })
            .expect("a valid camera")
    });
    let mut items = items.to_vec();
    if let Some(view) = view.as_ref().filter(|view| view.depth_sort()) {
        items.sort_by(|a, b| view.depth_of(*b).total_cmp(&view.depth_of(*a)));
    }
    let contents: Vec<ContentRect> = items
        .iter()
        .map(|g| content(*g, texels.size, crop))
        .collect();
    let background = BACKGROUND.map(|c| srgb_channel_to_linear(f32::from(c) / 255.0));
    let mut out = Reference {
        rgb: Vec::with_capacity((WIDTH * HEIGHT) as usize),
        coverage: Vec::with_capacity((WIDTH * HEIGHT) as usize),
    };
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut sum = [0.0f32; 3];
            let mut covered = 0u32;
            for sy in 0..n {
                for sx in 0..n {
                    let at = CanvasPoint {
                        x: x as f32 + (sx as f32 + 0.5) / n as f32,
                        y: y as f32 + (sy as f32 + 0.5) / n as f32,
                    };
                    let mut color = background;
                    let mut hit = false;
                    // Stacking order: the last item that shows a sample is the one seen.
                    for (geometry, content) in items.iter().zip(&contents) {
                        if let Some(c) = sample(*geometry, view.as_ref(), content, at, texels, crop)
                        {
                            color = c;
                            hit = true;
                        }
                    }
                    covered += u32::from(hit);
                    for i in 0..3 {
                        sum[i] += color[i];
                    }
                }
            }
            let samples = (n * n) as f32;
            out.rgb.push(sum.map(|s| encode(s / samples)));
            out.coverage.push(covered as f32 / samples);
        }
    }
    out
}

/// Linear light to an sRGB code value in `0..=255`, unrounded.
fn encode(linear: f32) -> f32 {
    let c = linear.clamp(0.0, 1.0);
    let srgb = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    srgb * 255.0
}

pub(super) fn gpu(
    compositor: &mut crate::Compositor,
    items: &[ItemGeometry],
    rgba: &[u8],
    size: u32,
    crop: Crop,
) -> Snapshot {
    let image = compositor
        .upload_rgba8(size, size, rgba)
        .expect("the pattern uploads");
    draw(compositor, items, &image, crop)
}

pub(super) fn draw(
    compositor: &mut crate::Compositor,
    items: &[ItemGeometry],
    image: &Image,
    crop: Crop,
) -> Snapshot {
    draw_seen(compositor, items, None, image, crop)
}

/// [`draw`] in a 3D scene with `camera` (ADR-0094).
fn draw_seen(
    compositor: &mut crate::Compositor,
    items: &[ItemGeometry],
    camera: Option<SceneCamera>,
    image: &Image,
    crop: Crop,
) -> Snapshot {
    let scene_items: Vec<_> = items
        .iter()
        .map(|g| crate::Item {
            camera,
            ..item(image, Placement::Geometry(*g), crop, 1.0)
        })
        .collect();
    let [r, g, b] = BACKGROUND.map(|c| f32::from(c) / 255.0);
    let scene = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
        background: Color::rgb(r, g, b),
        items: &scene_items,
    };
    compositor
        .render(&scene, true)
        .expect("a 3D scene renders")
        .pixels()
        .expect("readback was requested")
        .clone()
}

/// Mean and 99th-percentile error per channel over the pixels `keep` selects, and how
/// many there were.
pub(super) fn error(
    got: &Snapshot,
    want: &Reference,
    keep: impl Fn(f32) -> bool,
) -> (f32, f32, usize) {
    let mut errors = Vec::new();
    for (i, (rgb, coverage)) in want.rgb.iter().zip(&want.coverage).enumerate() {
        if !keep(*coverage) {
            continue;
        }
        let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
        let pixel = got.pixel(x, y).expect("inside");
        for c in 0..3 {
            errors.push((f32::from(pixel[c]) - rgb[c]).abs());
        }
    }
    assert!(!errors.is_empty(), "no pixels selected");
    let mean = errors.iter().sum::<f32>() / errors.len() as f32;
    errors.sort_by(f32::total_cmp);
    let p99 = errors[(errors.len() - 1) * 99 / 100];
    (mean, p99, errors.len() / 3)
}

pub(super) fn interior(coverage: f32) -> bool {
    coverage >= 0.999
}

fn edge(coverage: f32) -> bool {
    coverage > 0.0 && coverage < 0.999
}

fn all(_: f32) -> bool {
    true
}

pub(super) fn check(name: &str, measured: (f32, f32, usize), max_mean: f32, max_p99: f32) {
    let (mean, p99, pixels) = measured;
    let _ = writeln!(
        std::io::stderr(),
        "{name}: mean {mean:.2}, p99 {p99:.1} over {pixels} px"
    );
    assert!(
        mean <= max_mean && p99 <= max_p99,
        "{name}: mean {mean:.2} (at most {max_mean}), p99 {p99:.1} (at most {max_p99}) over {pixels} px"
    );
}

/// A: an item turned 50° with perspective. Interpolating the texture coordinate with `w`
/// keeps the checkerboard straight; drawn affinely, the spike measured a mean of 55.
#[test]
fn golden_3d_a_turned_item_is_perspective_correct() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let items = [geometry(
        (120.0, 68.0),
        (140.0, 105.0),
        0.0,
        0.0,
        50.0,
        0.0,
        Some(175.0),
    )];
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE, Crop::NONE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 8, Crop::NONE);
    check("A interior", error(&got, &want, interior), 3.0, 40.0);
}

/// B: tilted toward the viewer and pushed forward, so the top edge is behind the viewer
/// and ADR-0022's four corners can't draw it. The polygon clipped by the near plane can.
#[test]
fn golden_3d_b_an_item_reaching_behind_the_viewer_draws_its_front() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let items = [geometry(
        (120.0, 83.0),
        (175.0, 130.0),
        0.0,
        -70.0,
        0.0,
        62.5,
        Some(100.0),
    )];
    let behind = items[0]
        .project()
        .expect("valid")
        .corners
        .iter()
        .filter(|corner| matches!(corner, Placed::BehindViewer))
        .count();
    assert!(behind > 0, "the scenario has a corner behind the viewer");
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE, Crop::NONE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 8, Crop::NONE);
    check("B all", error(&got, &want, all), 2.0, 40.0);
}

/// D: a small rotated and turned item. Analytic coverage antialiases its edges; aliased,
/// the spike measured a mean of 36 on edge pixels, and 13 with 4× MSAA.
#[test]
fn golden_3d_d_edges_are_antialiased() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let items = [geometry(
        (120.0, 68.0),
        (90.0, 60.0),
        17.0,
        0.0,
        35.0,
        0.0,
        Some(225.0),
    )];
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE, Crop::NONE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 16, Crop::NONE);
    check("D edge", error(&got, &want, edge), 6.0, 40.0);
}

/// D cropped: a smaller item showing only part of its source, cropped just past a texel
/// ring. Its pushed-out fringe samples are clamped to the crop, and its chain holds only
/// the cropped texels, so the cropped-away ring doesn't show at its edges; the reference
/// clamps its bilinear samples to the crop the same way. On lavapipe the edge mean is
/// about 2.8; with the chain built from the whole source it was about 6.6, and 8.2
/// without the clamp either.
#[test]
fn golden_3d_d_a_cropped_item_shows_nothing_past_its_crop() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let items = [geometry(
        (120.0, 68.0),
        (45.0, 30.0),
        17.0,
        0.0,
        35.0,
        0.0,
        Some(225.0),
    )];
    let crop = Crop {
        left: 17,
        top: 17,
        right: 12,
        bottom: 12,
    };
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE, crop);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 16, crop);
    check("D cropped edge", error(&got, &want, edge), 5.0, 40.0);
}

/// C: a floor tilted 75°, minified about twice across and eight times along its depth.
/// Mips with anisotropic filtering keep the texel grid from aliasing. On lavapipe the
/// mean is about 6 with them, 18 with trilinear mips alone and 29 with one level, so the
/// limit fails without anisotropy. The pattern's one-texel rings are much finer than the
/// spike's 32-texel checkerboard, which measured 1.16, 3.22 and 4.80.
#[test]
fn golden_3d_c_a_minified_floor_samples_mips_anisotropically() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(LARGE_SOURCE);
    let items = [geometry(
        (120.0, 75.0),
        (225.0, 225.0),
        0.0,
        75.0,
        0.0,
        0.0,
        Some(250.0),
    )];
    let got = gpu(
        &mut opened.compositor,
        &items,
        &rgba,
        LARGE_SOURCE,
        Crop::NONE,
    );
    let want = reference(&items, &Texels::new(&rgba, LARGE_SOURCE), 16, Crop::NONE);
    check("C interior", error(&got, &want, interior), 8.0, 40.0);
}

/// An upload's chain is rebuilt after the upload is rewritten.
#[test]
fn a_rewritten_upload_rebuilds_its_mips() {
    let Some(mut opened) = open() else {
        return;
    };
    let solid = |rgb: [u8; 3]| -> Vec<u8> {
        std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], 64 * 64)
            .flatten()
            .collect()
    };
    let image = opened
        .compositor
        .upload_rgba8(64, 64, &solid([255, 0, 0]))
        .expect("uploads");
    let floor = [geometry(
        (120.0, 75.0),
        (225.0, 225.0),
        0.0,
        75.0,
        0.0,
        0.0,
        Some(250.0),
    )];
    let centre = |snapshot: &Snapshot| snapshot.pixel(120, 75).expect("inside");
    let red = draw(&mut opened.compositor, &floor, &image, Crop::NONE);
    assert_eq!(centre(&red)[..3], [255, 0, 0]);
    opened
        .compositor
        .rewrite_rgba8(&image, &solid([0, 255, 0]))
        .expect("rewrites");
    let green = draw(&mut opened.compositor, &floor, &image, Crop::NONE);
    assert_eq!(centre(&green)[..3], [0, 255, 0]);
    let again = draw(&mut opened.compositor, &floor, &image, Crop::NONE);
    assert_eq!(centre(&again)[..3], [0, 255, 0]);
}

/// A layer's chain is rebuilt after `render_into` draws into it, and not again while it
/// is unchanged, however many calls draw it in 3D.
#[test]
fn a_layer_rebuilds_its_mips_only_after_it_is_drawn_into() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let layer = compositor.layer(64, 64).expect("a layer");
    let fill = |compositor: &mut crate::Compositor, rgb: [f32; 3]| {
        let scene = Scene {
            canvas: FrameSize::new(64, 64).expect("non-zero"),
            background: Color::rgb(rgb[0], rgb[1], rgb[2]),
            items: &[],
        };
        compositor.render_into(&scene, &layer).expect("draws into");
    };
    let floor = [geometry(
        (120.0, 75.0),
        (225.0, 225.0),
        0.0,
        75.0,
        0.0,
        0.0,
        Some(250.0),
    )];
    let centre = |snapshot: &Snapshot| snapshot.pixel(120, 75).expect("inside");
    fill(compositor, [1.0, 0.0, 0.0]);
    let red = draw(compositor, &floor, &layer, Crop::NONE);
    assert_eq!(centre(&red)[..3], [255, 0, 0]);
    draw(compositor, &floor, &layer, Crop::NONE);
    assert_eq!(layer.mip_builds(), 1, "an unchanged layer is not rebuilt");
    fill(compositor, [0.0, 1.0, 0.0]);
    let green = draw(compositor, &floor, &layer, Crop::NONE);
    assert_eq!(centre(&green)[..3], [0, 255, 0]);
    assert_eq!(layer.mip_builds(), 2);
}

/// An import drawn in 3D by several calls in one cache frame, as several mixes draw a
/// capture, is rebuilt once per new frame (ADR-0050 decision 5).
#[test]
fn an_import_rebuilds_its_mips_once_per_frame() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let (frame, _) = super::counted_frame(compositor, 64, 64, [0, 0, 255, 255]);
    let mut cache = crate::ResourceCache::<&'static str>::new();
    let floor = [geometry(
        (120.0, 75.0),
        (225.0, 225.0),
        0.0,
        75.0,
        0.0,
        0.0,
        Some(250.0),
    )];
    for expected in 1..=2 {
        // The platform's next frame in the same texture, under a hold of its own.
        let frame = jstream_platform::GpuFrame::new(
            frame.texture().clone(),
            *frame.info(),
            std::sync::Arc::new(super::CountSync(std::sync::Arc::default())),
        );
        cache.begin_frame();
        for _mix in 0..2 {
            let image = cache
                .import(compositor, &"capture", &frame)
                .expect("imports");
            let shown = draw(compositor, &floor, image, Crop::NONE);
            assert_eq!(shown.pixel(120, 75).expect("inside")[..3], [0, 0, 255]);
        }
        let image = cache.get(&"capture").expect("cached");
        assert_eq!(image.mip_builds(), expected, "once per frame");
        drop(cache.end_frame(compositor));
    }
    drop(cache.drain());
}

/// E: two items through each other, turned 45° each way. They draw in stacking order
/// whatever their `z`: the upper one covers the lower one wherever they overlap.
#[test]
fn golden_3d_e_items_through_each_other_draw_in_stacking_order() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let items = [
        geometry(
            (120.0, 68.0),
            (105.0, 75.0),
            0.0,
            0.0,
            45.0,
            0.0,
            Some(200.0),
        ),
        geometry(
            (120.0, 68.0),
            (105.0, 75.0),
            0.0,
            0.0,
            -45.0,
            0.0,
            Some(200.0),
        ),
    ];
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE, Crop::NONE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 8, Crop::NONE);
    check("E all", error(&got, &want, all), 2.0, 40.0);
}

/// A source that is green inside `crop` and red outside it.
fn green_inside(size: u32, crop: Crop) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let inside =
                x >= crop.left && x < size - crop.right && y >= crop.top && y < size - crop.bottom;
            rgba.extend(if inside {
                [0, 200, 0, 255]
            } else {
                [255, 0, 0, 255]
            });
        }
    }
    rgba
}

/// A cropped item minified until it samples its chain's coarse levels shows nothing
/// from past its crop (#641): the chain is built from the cropped region, so no level
/// averages a texel outside it into the item. Built from the whole source, the coarse
/// levels and the anisotropic footprint pull the red border into the item's edges.
#[test]
fn golden_3d_a_cropped_item_minified_shows_nothing_past_its_crop() {
    let Some(mut opened) = open() else {
        return;
    };
    // A crop that doesn't fall on a power-of-two boundary, so every coarse texel at its
    // edge would mix both sides.
    let crop = Crop {
        left: 101,
        top: 93,
        right: 117,
        bottom: 109,
    };
    let rgba = green_inside(LARGE_SOURCE, crop);
    // A small floor tilted away: minified many times over, most along its depth.
    let items = [geometry(
        (120.0, 75.0),
        (60.0, 60.0),
        10.0,
        70.0,
        0.0,
        0.0,
        Some(250.0),
    )];
    let got = gpu(&mut opened.compositor, &items, &rgba, LARGE_SOURCE, crop);
    let want = reference(&items, &Texels::new(&rgba, LARGE_SOURCE), 4, crop);
    let mut covered = 0;
    let mut worst = 0u8;
    for (i, coverage) in want.coverage.iter().enumerate() {
        if *coverage == 0.0 {
            continue;
        }
        covered += 1;
        let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
        worst = worst.max(got.pixel(x, y).expect("inside")[0]);
    }
    assert!(covered > 50, "the item covers {covered} px");
    // Only the dark grey background (20) may show red, where an edge pixel blends with it.
    assert!(
        worst <= BACKGROUND[0] + 2,
        "red from past the crop reaches the item: red {worst} over {covered} px"
    );
}

/// A floor that samples coarse levels, for the rebuild tests.
fn floor() -> ItemGeometry {
    geometry(
        (120.0, 75.0),
        (225.0, 225.0),
        0.0,
        75.0,
        0.0,
        0.0,
        Some(250.0),
    )
}

/// A layer's chain is rebuilt once per change to the layer, not once per call that
/// draws it (#641): two items and two calls drawing an unchanged layer build it once,
/// and drawing into the layer again rebuilds it before its next 3D draw.
#[test]
fn a_layer_rebuilds_its_mips_once_per_change() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let red = super::solid(compositor, 64, 64, [255, 0, 0, 255]);
    let green = super::solid(compositor, 64, 64, [0, 255, 0, 255]);
    let layer = compositor.layer(64, 64).expect("layer");
    let fill = |compositor: &mut crate::Compositor, image: &Image| {
        let items = [item(image, Placement::IDENTITY, Crop::NONE, 1.0)];
        let scene = Scene {
            canvas: FrameSize::new(64, 64).expect("non-zero"),
            background: Color::TRANSPARENT,
            items: &items,
        };
        compositor
            .render_into(&scene, &layer)
            .expect("render into the layer");
    };
    let centre = |snapshot: &Snapshot| snapshot.pixel(120, 75).expect("inside")[..3].to_vec();
    fill(compositor, &red);
    let first = draw(compositor, &[floor(), floor()], &layer, Crop::NONE);
    let second = draw(compositor, &[floor()], &layer, Crop::NONE);
    assert_eq!(centre(&first), [255, 0, 0]);
    assert_eq!(centre(&second), [255, 0, 0]);
    assert_eq!(
        layer.chain_builds(),
        (1, 1),
        "built once for two items and two calls"
    );

    fill(compositor, &green);
    let third = draw(compositor, &[floor()], &layer, Crop::NONE);
    assert_eq!(centre(&third), [0, 255, 0], "the changed layer shows");
    assert_eq!(layer.chain_builds(), (1, 2), "rebuilt once for the change");
}

/// An import's chain is rebuilt when the cache gives it a new frame, not when the same
/// frame is shown again (#641).
#[test]
fn an_import_rebuilds_its_mips_only_for_a_new_frame() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let mut cache = crate::ResourceCache::<&'static str>::new();
    let (frame, _) = super::counted_frame(compositor, 64, 64, [255, 0, 0, 255]);
    let mut show = |compositor: &mut crate::Compositor, frame: &jstream_platform::GpuFrame| {
        cache.begin_frame();
        let image = cache.import(compositor, &"capture", frame).expect("import");
        let _ = draw(compositor, &[floor()], image, Crop::NONE);
        let builds = image.chain_builds();
        let _ = cache.end_frame(compositor);
        builds
    };
    assert_eq!(show(compositor, &frame), (1, 1));
    assert_eq!(
        show(compositor, &frame.clone()),
        (1, 1),
        "the same frame again"
    );
    // A new frame in the same texture: the platform wrote it again.
    let again = jstream_platform::GpuFrame::new(
        frame.texture().clone(),
        *frame.info(),
        std::sync::Arc::new(super::CountSync(std::sync::Arc::default())),
    );
    assert_eq!(show(compositor, &again), (1, 2), "a new frame rebuilds");
    // A new frame in the same texture under the same hold, as the Windows capture ring
    // hands out one hold per slot (#687 round 1 B2): only its time tells it apart.
    let mut info = *again.info();
    info.pts = jstream_media::MediaTime::new(info.pts.ticks() + 1, info.pts.timebase());
    let same_hold = jstream_platform::GpuFrame::new(
        again.texture().clone(),
        info,
        std::sync::Arc::clone(again.sync()),
    );
    assert_eq!(
        show(compositor, &same_hold),
        (1, 3),
        "a new frame under the same hold rebuilds"
    );
    assert_eq!(
        show(compositor, &same_hold.clone()),
        (1, 3),
        "and that frame again does not"
    );
}

/// Two items of one source with different crops each get a chain of their crop.
#[test]
fn each_crop_gets_its_own_chain() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let left = Crop {
        left: 0,
        top: 0,
        right: 32,
        bottom: 0,
    };
    let right = Crop {
        left: 32,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let mut rgba = Vec::new();
    for _ in 0..64 {
        for x in 0..64 {
            rgba.extend(if x < 32 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            });
        }
    }
    let image = compositor.upload_rgba8(64, 64, &rgba).expect("uploads");
    let items = [
        item(&image, Placement::Geometry(floor()), left, 1.0),
        item(
            &image,
            Placement::Geometry(geometry(
                (60.0, 40.0),
                (60.0, 60.0),
                0.0,
                60.0,
                0.0,
                0.0,
                Some(250.0),
            )),
            right,
            1.0,
        ),
    ];
    let [r, g, b] = BACKGROUND.map(|c| f32::from(c) / 255.0);
    let scene = Scene {
        canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
        background: Color::rgb(r, g, b),
        items: &items,
    };
    let snapshot = compositor
        .render(&scene, true)
        .expect("renders")
        .pixels()
        .expect("readback")
        .clone();
    assert_eq!(snapshot.pixel(120, 100).expect("inside")[..3], [255, 0, 0]);
    assert_eq!(snapshot.pixel(60, 40).expect("inside")[..3], [0, 0, 255]);
    assert_eq!(
        image.chain_builds(),
        (2, 2),
        "one chain per crop, each built once"
    );
}

/// A 3D item of an image with more texels than its extent (ADR-0073) builds its chain
/// from its crop in texels (`Region::in_texels`), and maps its texture coordinates by
/// the texels too (#791). The 128² image is placed as 64², cropped in those pixels, and
/// split inside the crop into a green left and a blue right, with red past the crop. A
/// crop read as texels, the extent and texels swapped, or a remap divided by the extent
/// each shows red, or the green half where the blue one belongs.
#[test]
fn a_dense_cropped_item_samples_only_its_crop_in_texels() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    const TEXELS: u32 = 2 * SOURCE;
    let crop = Crop {
        left: 17,
        top: 13,
        right: 12,
        bottom: 16,
    };
    // The crop in texels: every edge doubles.
    let (left, top) = (2 * crop.left, 2 * crop.top);
    let (right, bottom) = (TEXELS - 2 * crop.right, TEXELS - 2 * crop.bottom);
    let middle = (left + right) / 2;
    let mut rgba = Vec::with_capacity((TEXELS * TEXELS * 4) as usize);
    for y in 0..TEXELS {
        for x in 0..TEXELS {
            rgba.extend(
                if !(left..right).contains(&x) || !(top..bottom).contains(&y) {
                    [255, 0, 0, 255]
                } else if x < middle {
                    [0, 200, 0, 255]
                } else {
                    [0, 0, 220, 255]
                },
            );
        }
    }
    let mut image = compositor
        .upload_rgba8(TEXELS, TEXELS, &rgba)
        .expect("uploads");
    image.set_extent(FrameSize::new(SOURCE, SOURCE).expect("non-zero"));
    // Tilted a little, so it is drawn in 3D, and shown larger than its crop.
    let item = geometry(
        (120.0, 68.0),
        (100.0, 100.0),
        0.0,
        10.0,
        0.0,
        0.0,
        Some(250.0),
    );
    let got = draw(compositor, &[item], &image, crop);
    let near = |x: u32, want: [u8; 3]| {
        let pixel = got.pixel(x, 68).expect("inside");
        let close = pixel[..3]
            .iter()
            .zip(want)
            .all(|(got, want)| got.abs_diff(want) <= 2);
        assert!(close, "at x {x}: {pixel:?}, not {want:?}");
    };
    // The crop's left half on the item's left, its right half on the right.
    near(95, [0, 200, 0]);
    near(145, [0, 0, 220]);
    let mut worst = 0u8;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            worst = worst.max(got.pixel(x, y).expect("inside")[0]);
        }
    }
    // Only the dark grey background (20) has any red.
    assert!(
        worst <= BACKGROUND[0] + 2,
        "red from past the crop reaches the item: red {worst}"
    );
}

/// "Accepted means drawable" through the frame path itself (#629 round 2 B4): a scene of
/// the most extreme 3D items `check_drawable` accepts, on thin sources and an ordinary
/// one, renders, as `jstream-core`'s render thread calls it, instead of failing the frame.
#[test]
fn a_frame_of_the_most_extreme_accepted_3d_items_renders() {
    let Some(mut opened) = open() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let accepted: Vec<ItemGeometry> = crate::tests::extreme_geometries(20_000)
        .into_iter()
        .filter(|geometry| geometry.check_drawable().is_ok())
        .collect();
    assert!(accepted.len() > 1_000, "{} accepted", accepted.len());
    // The furthest-out ones by their largest input, and the bound cases at the front.
    let largest = |g: &ItemGeometry| {
        [
            g.center.x.get(),
            g.center.y.get(),
            g.size.width.get(),
            g.size.height.get(),
            g.depth_px.get(),
        ]
        .into_iter()
        .map(f32::abs)
        .fold(0.0, f32::max)
    };
    let mut extreme = accepted.clone();
    extreme.sort_by(|a, b| largest(b).total_cmp(&largest(a)));
    extreme.truncate(48);
    extreme.extend(accepted.iter().take(8));
    for (width, height) in [(8192, 1), (1, 8192), (64, 36)] {
        let rgba = vec![200u8; (width * height * 4) as usize];
        let image = compositor
            .upload_rgba8(width, height, &rgba)
            .expect("the source uploads");
        for chunk in extreme.chunks(8) {
            let items: Vec<_> = chunk
                .iter()
                .map(|g| item(&image, Placement::Geometry(*g), Crop::NONE, 1.0))
                .collect();
            let scene = Scene {
                canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
                background: Color::rgb(0.0, 0.0, 0.0),
                items: &items,
            };
            compositor
                .render(&scene, false)
                .unwrap_or_else(|err| panic!("{width}x{height} {chunk:?}: {err}"));
        }
    }
}

/// A camera on the 240×136 test canvas: from `position`, looking at `target`.
fn camera(position: (f32, f32, f32), target: (f32, f32, f32), fov: f32) -> SceneCamera {
    SceneCamera {
        position: Vec3::new(position.0, position.1, position.2).expect("finite"),
        target: Vec3::new(target.0, target.1, target.2).expect("finite"),
        fov_deg: Finite32::new(fov).expect("finite"),
        roll_deg: Finite32::ZERO,
        depth_sort: true,
        focus_px: None,
        blur_px: Finite32::ZERO,
    }
}

/// ADR-0094: flat items seen through the framing camera draw where they draw in a 2D
/// scene, so switching a scene to 3D moves nothing.
#[test]
fn golden_camera_framing_draws_flat_items_on_their_2d_boxes() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let image = opened
        .compositor
        .upload_rgba8(SOURCE, SOURCE, &rgba)
        .expect("the pattern uploads");
    let items = [
        geometry((70.0, 50.0), (80.0, 60.0), 0.0, 0.0, 0.0, 0.0, None),
        geometry((160.0, 80.0), (90.0, 70.0), 20.0, 0.0, 0.0, 0.0, None),
    ];
    let framing = SceneCamera::framing(jstream_protocol::Canvas {
        width: WIDTH,
        height: HEIGHT,
    });
    let flat = draw(&mut opened.compositor, &items, &image, Crop::NONE);
    let seen = draw_seen(
        &mut opened.compositor,
        &items,
        Some(framing),
        &image,
        Crop::NONE,
    );
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 8, Crop::NONE);
    // Bounded by the 2D draw's own error against the reference on this device, which
    // varies with the device's sampling: on lavapipe the 2D draw measures a mean of 3.47
    // and a p99 of 41.8 and the framing camera's 2.31 and 21.1, and Metal measured the
    // framing camera's at 3.46 and 41.8.
    let (flat_mean, flat_p99, _) = error(&flat, &want, interior);
    let _ = writeln!(
        std::io::stderr(),
        "framing 2D reference: mean {flat_mean:.2}, p99 {flat_p99:.1}"
    );
    check(
        "framing interior",
        error(&seen, &want, interior),
        (flat_mean + 1.5).max(3.0),
        (flat_p99 + 25.0).max(40.0),
    );
    // The same pixels as the 2D scene inside the items, up to sampling.
    let mut differ = 0;
    for (i, coverage) in want.coverage.iter().enumerate() {
        let (x, y) = (i as u32 % WIDTH, i as u32 / WIDTH);
        let (a, b) = (flat.pixel(x, y).expect("in"), seen.pixel(x, y).expect("in"));
        let far = (0..3).any(|c| a[c].abs_diff(b[c]) > 48);
        if *coverage >= 0.999 && far {
            differ += 1;
        }
    }
    assert!(differ < 40, "{differ} interior pixels moved");
}

/// ADR-0094: a set seen from off its axis, a back wall and a floor, perspective-correct
/// against the CPU reference through the same camera.
#[test]
fn golden_camera_an_off_axis_set_is_perspective_correct() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let items = [
        // The back wall, pushed away.
        geometry((120.0, 60.0), (150.0, 90.0), 0.0, 0.0, 0.0, -120.0, None),
        // The floor, flat, reaching toward the camera.
        geometry((120.0, 105.0), (150.0, 220.0), 0.0, 90.0, 0.0, -10.0, None),
    ];
    let seen = camera((40.0, 20.0, 260.0), (120.0, 80.0, -60.0), 50.0);
    let image = opened
        .compositor
        .upload_rgba8(SOURCE, SOURCE, &rgba)
        .expect("the pattern uploads");
    let got = draw_seen(
        &mut opened.compositor,
        &items,
        Some(seen),
        &image,
        Crop::NONE,
    );
    let want = reference_seen(
        &items,
        Some(seen),
        &Texels::new(&rgba, SOURCE),
        16,
        Crop::NONE,
    );
    // The floor is minified toward the horizon, as in scenario C, so the bound is C's
    // kind rather than A's; a wrong projection misses by tens of steps.
    check("set interior", error(&got, &want, interior), 6.0, 40.0);
    check("set all", error(&got, &want, all), 6.0, 60.0);
}

/// ADR-0094 decision 4: a 3D scene draws far to near whatever the stacking order, and
/// with `depth_sort` off in stacking order, as are items in another `depth_run`.
#[test]
fn golden_camera_items_draw_far_to_near() {
    let Some(mut opened) = open() else {
        return;
    };
    let near_rgba = pattern(SOURCE);
    let far_rgba: Vec<u8> = near_rgba
        .chunks_exact(4)
        .flat_map(|_| [30, 200, 60, 255])
        .collect();
    let compositor = &mut opened.compositor;
    let near_image = compositor
        .upload_rgba8(SOURCE, SOURCE, &near_rgba)
        .expect("uploads");
    let far_image = compositor
        .upload_rgba8(SOURCE, SOURCE, &far_rgba)
        .expect("uploads");
    let near = geometry((120.0, 68.0), (60.0, 60.0), 0.0, 0.0, 0.0, 40.0, None);
    let far = geometry((120.0, 68.0), (160.0, 110.0), 0.0, 0.0, 0.0, -80.0, None);
    let mut seen = camera((120.0, 68.0, 300.0), (120.0, 68.0, 0.0), 45.0);
    let centre = |seen: SceneCamera, far_run: u32, compositor: &mut crate::Compositor| {
        // The near item is first in stacking order, so the far one is on top of it.
        let items = [
            crate::Item {
                camera: Some(seen),
                ..item(&near_image, Placement::Geometry(near), Crop::NONE, 1.0)
            },
            crate::Item {
                camera: Some(seen),
                depth_run: far_run,
                ..item(&far_image, Placement::Geometry(far), Crop::NONE, 1.0)
            },
        ];
        let scene = Scene {
            canvas: FrameSize::new(WIDTH, HEIGHT).expect("non-zero"),
            background: Color::rgb(0.0, 0.0, 0.0),
            items: &items,
        };
        compositor
            .render(&scene, true)
            .expect("renders")
            .pixels()
            .expect("readback")
            .pixel(WIDTH / 2 + 3, HEIGHT / 2 + 3)
            .expect("inside")
    };
    let sorted = centre(seen, 0, compositor);
    assert_ne!(
        sorted[..3],
        [30, 200, 60],
        "the near item is drawn over the far one"
    );
    let apart = centre(seen, 1, compositor);
    assert_eq!(
        apart[..3],
        [30, 200, 60],
        "an item in a run of its own (a stinger's) stays on top through an equal camera"
    );
    seen.depth_sort = false;
    let stacked = centre(seen, 0, compositor);
    assert_eq!(
        stacked[..3],
        [30, 200, 60],
        "stacking order puts the far item on top"
    );
}

/// Mean linear light, in code-value units (0 to 255), and its standard deviation over
/// the canvas pixels `keep` selects. Linear, since the blur averages light: averaging
/// code values would read a correct blur as brighter.
fn luminance_stats(snapshot: &Snapshot, keep: impl Fn(u32, u32) -> bool) -> (f32, f32) {
    let linear = |c: u8| {
        let v = f32::from(c) / 255.0;
        let l = if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        };
        l * 255.0
    };
    let mut values = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if keep(x, y) {
                let p = snapshot.pixel(x, y).expect("inside");
                values.push((linear(p[0]) + linear(p[1]) + linear(p[2])) / 3.0);
            }
        }
    }
    let n = values.len() as f32;
    let mean = values.iter().sum::<f32>() / n;
    let var = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
    (mean, var.sqrt())
}

/// How many pixels of row `y` between `x0` and `x1` are part way between the background
/// and the item: the width of the item's edge there.
fn edge_width(snapshot: &Snapshot, y: u32, x0: u32, x1: u32, inside: f32) -> usize {
    let background = 255.0 * (f32::from(BACKGROUND[0]) / 255.0).powf(2.2);
    (x0..x1)
        .filter(|&x| {
            let p = snapshot.pixel(x, y).expect("inside");
            let l = (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 3.0;
            let l = 255.0 * (l / 255.0).powf(2.2);
            l > background + 0.15 * (inside - background)
                && l < inside - 0.15 * (inside - background)
        })
        .count()
}

/// ADR-0108 decisions 2 to 4: an item on the focus plane draws as it does without depth
/// of field; one off it loses its pattern's contrast but keeps its mean colour, and its
/// edge softens over about the blur's diameter.
#[test]
fn golden_dof_blurs_by_distance_from_the_focus_plane() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let image = opened
        .compositor
        .upload_rgba8(SOURCE, SOURCE, &rgba)
        .expect("the pattern uploads");
    // 300 px in front of the camera: about 55 × 44 canvas pixels around the centre.
    let items = [geometry(
        (120.0, 68.0),
        (100.0, 80.0),
        0.0,
        0.0,
        0.0,
        0.0,
        None,
    )];
    let sharp_camera = camera((120.0, 68.0, 300.0), (120.0, 68.0, 0.0), 45.0);
    let compositor = &mut opened.compositor;
    let sharp = draw_seen(compositor, &items, Some(sharp_camera), &image, Crop::NONE);

    let focused_camera = SceneCamera {
        focus_px: Finite32::new(300.0),
        blur_px: Finite32::new(12.0).expect("finite"),
        ..sharp_camera
    };
    let focused = draw_seen(compositor, &items, Some(focused_camera), &image, Crop::NONE);
    let mut most = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (a, b) = (
                sharp.pixel(x, y).expect("in"),
                focused.pixel(x, y).expect("in"),
            );
            most = most.max((0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0));
        }
    }
    assert!(
        most <= 2,
        "in focus, the item draws as without depth of field: {most}"
    );

    // Focused at 100 px, the item 300 px away blurs by 12 · 200 / 300 = 8 px.
    let defocused_camera = SceneCamera {
        focus_px: Finite32::new(100.0),
        ..focused_camera
    };
    let blurred = draw_seen(
        compositor,
        &items,
        Some(defocused_camera),
        &image,
        Crop::NONE,
    );
    let inner = |x: u32, y: u32| (105..=135).contains(&x) && (58..=78).contains(&y);
    let (sharp_mean, sharp_spread) = luminance_stats(&sharp, inner);
    let (blurred_mean, blurred_spread) = luminance_stats(&blurred, inner);
    let _ = writeln!(
        std::io::stderr(),
        "dof inner: sharp {sharp_mean:.1} ± {sharp_spread:.1}, blurred {blurred_mean:.1} ± {blurred_spread:.1}"
    );
    assert!(
        blurred_spread < 0.5 * sharp_spread,
        "the pattern blurs: {blurred_spread} against {sharp_spread}"
    );
    assert!(
        (blurred_mean - sharp_mean).abs() < 8.0,
        "the blur keeps the mean: {blurred_mean} against {sharp_mean}"
    );
    // The right edge, near x = 147, across row 68.
    let sharp_edge = edge_width(&sharp, 68, 125, 175, sharp_mean);
    let soft_edge = edge_width(&blurred, 68, 125, 175, blurred_mean);
    let _ = writeln!(
        std::io::stderr(),
        "dof edge: sharp {sharp_edge} px, blurred {soft_edge} px"
    );
    assert!(
        sharp_edge <= 3,
        "a sharp edge is a pixel or two: {sharp_edge}"
    );
    assert!(
        (7..=20).contains(&soft_edge),
        "a blur of 8 px softens the edge over about its diameter: {soft_edge}"
    );
}

/// ADR-0108 decision 2: of two items, the one on the focus plane stays sharp and the one
/// behind it blurs, in one scene.
#[test]
fn golden_dof_only_the_item_off_focus_blurs() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let image = opened
        .compositor
        .upload_rgba8(SOURCE, SOURCE, &rgba)
        .expect("the pattern uploads");
    let items = [
        // Behind, on the left; in front, at the focus, on the right.
        geometry((70.0, 68.0), (90.0, 90.0), 0.0, 0.0, 0.0, -300.0, None),
        geometry((175.0, 68.0), (50.0, 50.0), 0.0, 0.0, 0.0, 0.0, None),
    ];
    let base = camera((120.0, 68.0, 300.0), (120.0, 68.0, 0.0), 45.0);
    let compositor = &mut opened.compositor;
    let sharp = draw_seen(compositor, &items, Some(base), &image, Crop::NONE);
    let dof = SceneCamera {
        focus_px: Finite32::new(300.0),
        blur_px: Finite32::new(16.0).expect("finite"),
        ..base
    };
    let got = draw_seen(compositor, &items, Some(dof), &image, Crop::NONE);
    // The far item, 600 px away, blurs by 16 · 300 / 600 = 8 px; the near one not at all.
    // On the canvas, the far item covers about x 94 to 119 and the near one 137 to 164.
    let far = |x: u32, y: u32| (100..=113).contains(&x) && (63..=73).contains(&y);
    let near = |x: u32, y: u32| (141..=160).contains(&x) && (58..=78).contains(&y);
    let (_, far_sharp) = luminance_stats(&sharp, far);
    let (_, far_got) = luminance_stats(&got, far);
    let (_, near_sharp) = luminance_stats(&sharp, near);
    let (_, near_got) = luminance_stats(&got, near);
    let _ = writeln!(
        std::io::stderr(),
        "dof two items: far {far_sharp:.1} -> {far_got:.1}, near {near_sharp:.1} -> {near_got:.1}"
    );
    assert!(far_got < 0.6 * far_sharp, "the far item blurs");
    assert!(
        (near_got - near_sharp).abs() < 1.0,
        "the near item stays sharp"
    );
}

/// The pixels just outside a blurred item in `rows`, as a share of the item's light over
/// the background: the largest such share among pixels with a background neighbour,
/// which is small where the blur falls off softly and large where its edge is cut.
fn hardest_edge(snapshot: &Snapshot, rows: std::ops::Range<u32>) -> f32 {
    let linear = |c: u8| srgb_channel_to_linear(f32::from(c) / 255.0);
    let background = linear(BACKGROUND[0]);
    let lift = |x: u32, y: u32| {
        let p = snapshot.pixel(x, y).expect("inside");
        ((linear(p[0]) + linear(p[1]) + linear(p[2])) / 3.0 - background) / (1.0 - background)
    };
    let mut hardest: f32 = 0.0;
    for y in rows.start.max(1)..rows.end.min(HEIGHT - 1) {
        for x in 1..WIDTH - 1 {
            let here = lift(x, y);
            let beside = [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)];
            if beside.iter().any(|&(bx, by)| lift(bx, by) < 0.002) {
                hardest = hardest.max(here);
            }
        }
    }
    hardest
}

/// ADR-0108 decision 4 (#1122 review N1): every vertex of a tilted plane is pushed out by
/// its largest blur and no fragment blurs more, so the far edge falls off to the
/// background with no hard cut. Fails with each vertex pushed by its own blur, or with the
/// blur past the edge uncapped (both cut at about 0.18).
#[test]
fn golden_dof_a_tilted_plane_falls_off_softly() {
    let Some(mut opened) = open() else {
        return;
    };
    let white = vec![255; (SOURCE * SOURCE * 4) as usize];
    let image = opened
        .compositor
        .upload_rgba8(SOURCE, SOURCE, &white)
        .expect("the source uploads");
    let items = [geometry(
        (120.0, 68.0),
        (100.0, 160.0),
        0.0,
        60.0,
        0.0,
        0.0,
        None,
    )];
    let base = camera((120.0, 68.0, 300.0), (120.0, 68.0, 0.0), 45.0);
    let compositor = &mut opened.compositor;
    let sharp = draw_seen(compositor, &items, Some(base), &image, Crop::NONE);
    let dof = SceneCamera {
        focus_px: Finite32::new(232.0),
        blur_px: Finite32::new(64.0).expect("finite"),
        ..base
    };
    let got = draw_seen(compositor, &items, Some(dof), &image, Crop::NONE);
    // The plane's far half, above the canvas's middle, blurs by up to
    // 64 · (369 - 232) / 369 ≈ 24 px; the near edge, at the focus, stays sharp.
    let far = 0..HEIGHT / 2;
    let (sharp_edge, got_edge) = (hardest_edge(&sharp, far.clone()), hardest_edge(&got, far));
    let _ = writeln!(
        std::io::stderr(),
        "dof tilted edge: sharp {sharp_edge:.3}, blurred {got_edge:.3}"
    );
    assert!(
        sharp_edge > 0.5,
        "the sharp plane has a hard edge: {sharp_edge}"
    );
    assert!(
        got_edge < 0.1,
        "the blurred plane falls off softly: {got_edge}"
    );
}

/// ADR-0108 decision 2 (#1122 review N2): the blur follows each fragment's own distance,
/// so a tilted plane focused at its near end stays sharp there and blurs along itself:
/// the far band loses its pattern's contrast, the near band keeps it.
#[test]
fn golden_dof_a_tilted_plane_blurs_along_itself() {
    let Some(mut opened) = open() else {
        return;
    };
    let rgba = pattern(SOURCE);
    let image = opened
        .compositor
        .upload_rgba8(SOURCE, SOURCE, &rgba)
        .expect("the pattern uploads");
    let items = [geometry(
        (120.0, 68.0),
        (100.0, 160.0),
        0.0,
        60.0,
        0.0,
        0.0,
        None,
    )];
    let base = camera((120.0, 68.0, 300.0), (120.0, 68.0, 0.0), 45.0);
    let compositor = &mut opened.compositor;
    let sharp = draw_seen(compositor, &items, Some(base), &image, Crop::NONE);
    let dof = SceneCamera {
        focus_px: Finite32::new(232.0),
        blur_px: Finite32::new(24.0).expect("finite"),
        ..base
    };
    let got = draw_seen(compositor, &items, Some(dof), &image, Crop::NONE);
    // The near end, at the focus, is at the bottom of the canvas; the far end, about
    // 370 px away, blurs by about 24 · 138 / 370 ≈ 9 px. Each band stays inside the plane.
    let near = |x: u32, y: u32| (100..=140).contains(&x) && (89..=94).contains(&y);
    let far = |x: u32, y: u32| (105..=135).contains(&x) && (52..=58).contains(&y);
    let (_, near_sharp) = luminance_stats(&sharp, near);
    let (_, near_got) = luminance_stats(&got, near);
    let (_, far_sharp) = luminance_stats(&sharp, far);
    let (_, far_got) = luminance_stats(&got, far);
    let _ = writeln!(
        std::io::stderr(),
        "dof tilted: near {near_sharp:.1} -> {near_got:.1}, far {far_sharp:.1} -> {far_got:.1}"
    );
    assert!(far_got < 0.5 * far_sharp, "the far band blurs");
    assert!(near_got > 0.8 * near_sharp, "the near band stays sharp");
}
