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
    CanvasPoint, ContentRect, Crop as ProtocolCrop, Finite32, FitMode, ItemGeometry, NEAR_W,
    Placed, Size, Vec2,
};

use super::{item, open};
use crate::color::srgb_channel_to_linear;
use crate::{Color, Crop, Image, Placement, Scene, Snapshot};

const WIDTH: u32 = 240;
const HEIGHT: u32 = 136;
const SOURCE: u32 = 64;
/// Scenario C's source: big enough that the floor is minified several times over.
const LARGE_SOURCE: u32 = 512;
/// The background, dark grey so an item's edge has contrast against it.
const BACKGROUND: [u8; 3] = [20, 20, 20];

/// A `size`² pattern: an 8-texel checkerboard of two colours, with a brighter texel ring
/// every 16, so a bent or shifted texture shows, and a minified one aliases.
fn pattern(size: u32) -> Vec<u8> {
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

fn geometry(
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
struct Reference {
    rgb: Vec<[f32; 3]>,
    coverage: Vec<f32>,
}

/// The pattern's texels in linear light.
struct Texels {
    size: u32,
    linear: Vec<[f32; 3]>,
}

impl Texels {
    fn new(rgba: &[u8], size: u32) -> Self {
        Self {
            size,
            linear: rgba
                .chunks_exact(4)
                .map(|p| [0, 1, 2].map(|c| srgb_channel_to_linear(f32::from(p[c]) / 255.0)))
                .collect(),
        }
    }

    /// Bilinear, clamped to the edge, with texel centres at `(i + 0.5) / size`, as the
    /// compositor's sampler.
    fn bilinear(&self, u: f32, v: f32) -> [f32; 3] {
        let size = self.size as f32;
        let x = (u * size - 0.5).clamp(0.0, size - 1.0);
        let y = (v * size - 0.5).clamp(0.0, size - 1.0);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |x: f32, y: f32| {
            let (x, y) = (x.min(size - 1.0) as usize, y.min(size - 1.0) as usize);
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

fn content(geometry: ItemGeometry, size: u32) -> ContentRect {
    geometry
        .content(size, size, ProtocolCrop::default())
        .expect("valid")
        .expect("pixels")
}

/// What `geometry` shows at a canvas point, in linear light, or `None` outside it.
fn sample(
    geometry: ItemGeometry,
    content: &ContentRect,
    at: CanvasPoint,
    texels: &Texels,
) -> Option<[f32; 3]> {
    let local = geometry.unplace(at).expect("valid")?;
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
    let vertex = geometry.drawn_vertex(content, local).expect("valid")?;
    debug_assert!(vertex.w >= NEAR_W);
    Some(texels.bilinear(vertex.uv[0], vertex.uv[1]))
}

fn reference(items: &[ItemGeometry], texels: &Texels, n: u32) -> Reference {
    let contents: Vec<ContentRect> = items.iter().map(|g| content(*g, texels.size)).collect();
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
                        if let Some(c) = sample(*geometry, content, at, texels) {
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

fn gpu(
    compositor: &mut crate::Compositor,
    items: &[ItemGeometry],
    rgba: &[u8],
    size: u32,
) -> Snapshot {
    let image = compositor
        .upload_rgba8(size, size, rgba)
        .expect("the pattern uploads");
    draw(compositor, items, &image)
}

fn draw(compositor: &mut crate::Compositor, items: &[ItemGeometry], image: &Image) -> Snapshot {
    let scene_items: Vec<_> = items
        .iter()
        .map(|g| item(image, Placement::Geometry(*g), Crop::NONE, 1.0))
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
fn error(got: &Snapshot, want: &Reference, keep: impl Fn(f32) -> bool) -> (f32, f32, usize) {
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

fn interior(coverage: f32) -> bool {
    coverage >= 0.999
}

fn edge(coverage: f32) -> bool {
    coverage > 0.0 && coverage < 0.999
}

fn all(_: f32) -> bool {
    true
}

fn check(name: &str, measured: (f32, f32, usize), max_mean: f32, max_p99: f32) {
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
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 8);
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
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 8);
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
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 16);
    check("D edge", error(&got, &want, edge), 6.0, 40.0);
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
    let got = gpu(&mut opened.compositor, &items, &rgba, LARGE_SOURCE);
    let want = reference(&items, &Texels::new(&rgba, LARGE_SOURCE), 16);
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
    let red = draw(&mut opened.compositor, &floor, &image);
    assert_eq!(centre(&red)[..3], [255, 0, 0]);
    opened
        .compositor
        .rewrite_rgba8(&image, &solid([0, 255, 0]))
        .expect("rewrites");
    let green = draw(&mut opened.compositor, &floor, &image);
    assert_eq!(centre(&green)[..3], [0, 255, 0]);
    let again = draw(&mut opened.compositor, &floor, &image);
    assert_eq!(centre(&again)[..3], [0, 255, 0]);
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
    let got = gpu(&mut opened.compositor, &items, &rgba, SOURCE);
    let want = reference(&items, &Texels::new(&rgba, SOURCE), 8);
    check("E all", error(&got, &want, all), 2.0, 40.0);
}
