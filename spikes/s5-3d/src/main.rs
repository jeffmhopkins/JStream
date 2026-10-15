//! Spike S5 (3D1, ROADMAP M3): can the one-scene compositor draw ADR-0022's 3D fields
//! correctly with one textured polygon per item? See README.md for the questions.
//!
//! Usage: `cargo run --release [-- --hardware] [--out DIR]`. Writes PNGs and
//! `results.md` into `out/` (or DIR) and prints the results table.

mod geom;
mod gpu;
mod image;
mod reference;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use jstream_protocol::{Crop, Finite32, FitMode, ItemGeometry, Placed, Size, Vec2};

use crate::geom::{edges, expand, polygon};
use crate::gpu::{Draw, Gpu, Options};
use crate::image::{Image, compare, diff_image, quantize_like_gpu, write_png};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;
const BACKGROUND: [f32; 3] = [0.02, 0.02, 0.02];

#[allow(clippy::too_many_arguments)]
fn geometry(
    center: (f32, f32),
    size: (f32, f32),
    rotation: f32,
    tilt: f32,
    turn: f32,
    depth: f32,
    perspective: Option<f32>,
) -> ItemGeometry {
    let mut g = ItemGeometry::try_new(
        Vec2::new(center.0, center.1).expect("finite"),
        Size::try_new(size.0, size.1).expect("positive"),
    )
    .expect("valid");
    g.fit_mode = FitMode::Stretch;
    g.rotation_deg = Finite32::new(rotation).expect("finite");
    g.tilt_deg = Finite32::new(tilt).expect("finite");
    g.turn_deg = Finite32::new(turn).expect("finite");
    g.depth_px = Finite32::new(depth).expect("finite");
    g.perspective_px = perspective.map(|p| Finite32::new(p).expect("finite"));
    g
}

/// How the GPU path builds an item's vertices.
#[derive(Clone, Copy, PartialEq)]
enum Path3d {
    /// Homogeneous w per vertex: perspective-correct.
    Correct,
    /// The same projected corners with w = 1: what the 2D compositor would do with the
    /// projected quad.
    Affine,
}

fn draw(g: &ItemGeometry, size: u32, analytic: bool, path: Path3d) -> Option<Draw> {
    let content = g.content(size, size, Crop::default()).ok()??;
    let poly = polygon(g, &content)?;
    let mut vertices = if analytic {
        expand(g, &content, &poly, 1.0)
    } else {
        poly.clone()
    };
    if path == Path3d::Affine {
        for v in &mut vertices {
            v.w = 1.0;
        }
    }
    Some(Draw {
        vertices,
        edges: if analytic { edges(&poly) } else { Vec::new() },
    })
}

struct Run<'a> {
    gpu: &'a Gpu,
    out: PathBuf,
    levels: Vec<Image>,
    texture_mips: wgpu::Texture,
    texture_flat: wgpu::Texture,
    report: String,
}

impl Run<'_> {
    fn reference(&self, name: &str, items: &[ItemGeometry], n: u32) -> reference::Frame {
        let items: Vec<reference::Item> = items
            .iter()
            .filter_map(|g| {
                Some(reference::Item {
                    geometry: *g,
                    content: g.content(1024, 1024, Crop::default()).ok()??,
                })
            })
            .collect();
        let started = Instant::now();
        let frame = reference::render(WIDTH, HEIGHT, BACKGROUND, &items, &self.levels[0], n);
        eprintln!("{name}: reference {n}x{n} in {:?}", started.elapsed());
        write_png(
            &self.png(&format!("{name}-reference")),
            WIDTH,
            HEIGHT,
            &frame.rgba,
        );
        frame
    }

    fn gpu(&self, name: &str, items: &[ItemGeometry], options: Options, path: Path3d) -> Vec<u8> {
        let draws: Vec<Draw> = items
            .iter()
            .filter_map(|g| draw(g, 1024, options.analytic, path))
            .collect();
        let texture = if options.mips {
            &self.texture_mips
        } else {
            &self.texture_flat
        };
        let rgba = self
            .gpu
            .render(WIDTH, HEIGHT, BACKGROUND, texture, &draws, options);
        write_png(&self.png(name), WIDTH, HEIGHT, &rgba);
        rgba
    }

    fn png(&self, name: &str) -> PathBuf {
        self.out.join(format!("{name}.png"))
    }

    fn row(
        &mut self,
        scenario: &str,
        variant: &str,
        mask: &str,
        got: &[u8],
        want: &reference::Frame,
        m: &[bool],
    ) {
        let e = compare(got, &want.rgba, m);
        writeln!(
            self.report,
            "| {scenario} | {variant} | {mask} ({} px) | {:.2} | {} | {} |",
            e.pixels, e.mean, e.p99, e.max
        )
        .expect("string");
        let name = format!("{scenario}-{variant}-diff").replace(' ', "-");
        write_png(
            &self.png(&name),
            WIDTH,
            HEIGHT,
            &diff_image(got, &want.rgba),
        );
    }
}

fn masks(frame: &reference::Frame) -> (Vec<bool>, Vec<bool>, Vec<bool>) {
    let interior = frame.coverage.iter().map(|&c| c >= 0.999).collect();
    let edge = frame
        .coverage
        .iter()
        .map(|&c| c > 0.0 && c < 0.999)
        .collect();
    let all = vec![true; frame.coverage.len()];
    (interior, edge, all)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hardware = args.iter().any(|a| a == "--hardware");
    let out = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map_or_else(|| PathBuf::from("out"), PathBuf::from);
    std::fs::create_dir_all(&out).expect("out dir");

    let gpu = Gpu::open(!hardware);
    let source = quantize_like_gpu(&Image::pattern());
    // The reference samples level 0 only; mips are built from the same quantised texels.
    let levels: Vec<Image> = source.mips().iter().map(quantize_like_gpu).collect();
    let texture_mips = gpu.upload(&levels, true);
    let texture_flat = gpu.upload(&levels, false);
    let mut run = Run {
        gpu: &gpu,
        out: out.clone(),
        levels,
        texture_mips,
        texture_flat,
        report: String::new(),
    };
    let best = Options {
        mips: true,
        anisotropy: 16,
        msaa: 1,
        analytic: true,
    };
    writeln!(
        run.report,
        "Adapter: {}. Canvas {WIDTH}x{HEIGHT}, source 1024x1024. Errors are per channel in sRGB 8-bit steps against the CPU reference.\n",
        gpu.description()
    )
    .expect("string");
    writeln!(
        run.report,
        "| Scenario | Variant | Pixels | Mean | p99 | Max |"
    )
    .expect("string");
    writeln!(run.report, "|---|---|---|---|---|---|").expect("string");

    // A. Perspective-correct texturing: a turned item, drawn with w per vertex and with
    // the same corners but w = 1.
    let a = [geometry(
        (480.0, 270.0),
        (560.0, 420.0),
        0.0,
        0.0,
        50.0,
        0.0,
        Some(700.0),
    )];
    let reference_a = run.reference("a-perspective", &a, 8);
    let (interior, _, _) = masks(&reference_a);
    let got = run.gpu("a-perspective-correct", &a, best, Path3d::Correct);
    run.row(
        "A perspective",
        "w per vertex",
        "interior",
        &got,
        &reference_a,
        &interior,
    );
    let got = run.gpu("a-perspective-affine", &a, best, Path3d::Affine);
    run.row(
        "A perspective",
        "affine (w = 1)",
        "interior",
        &got,
        &reference_a,
        &interior,
    );

    // B. Near plane: tilted toward the viewer and pushed forward, so the top edge is
    // behind the viewer. ADR-0022's four corners can't draw it.
    let b = [geometry(
        (480.0, 330.0),
        (700.0, 520.0),
        0.0,
        -70.0,
        0.0,
        250.0,
        Some(400.0),
    )];
    let behind = b[0]
        .project()
        .expect("valid")
        .corners
        .iter()
        .filter(|c| matches!(c, Placed::BehindViewer))
        .count();
    let reference_b = run.reference("b-near-clip", &b, 8);
    let (_, _, all) = masks(&reference_b);
    let got = run.gpu("b-near-clip", &b, best, Path3d::Correct);
    run.row(
        "B near clip",
        "clipped in local frame",
        "all",
        &got,
        &reference_b,
        &all,
    );

    // C. Minification: a floor tilted 75°, about 4 texels per pixel vertically and 1
    // horizontally.
    let c = [geometry(
        (480.0, 300.0),
        (900.0, 900.0),
        0.0,
        75.0,
        0.0,
        0.0,
        Some(1000.0),
    )];
    let reference_c = run.reference("c-minify", &c, 16);
    let (interior, _, _) = masks(&reference_c);
    for (variant, options) in [
        (
            "no mips",
            Options {
                mips: false,
                anisotropy: 1,
                ..best
            },
        ),
        (
            "trilinear",
            Options {
                anisotropy: 1,
                ..best
            },
        ),
        ("trilinear + 16x aniso", best),
    ] {
        let name = format!("c-minify-{}", variant.replace([' ', '+'], ""));
        let got = run.gpu(&name, &c, options, Path3d::Correct);
        run.row(
            "C minify",
            variant,
            "interior",
            &got,
            &reference_c,
            &interior,
        );
    }

    // D. Edges: a small rotated and turned item.
    let d = [geometry(
        (480.0, 270.0),
        (360.0, 240.0),
        17.0,
        0.0,
        35.0,
        0.0,
        Some(900.0),
    )];
    let reference_d = run.reference("d-edges", &d, 16);
    let (_, edge, _) = masks(&reference_d);
    for (variant, options) in [
        (
            "aliased",
            Options {
                analytic: false,
                ..best
            },
        ),
        (
            "4x MSAA",
            Options {
                analytic: false,
                msaa: 4,
                ..best
            },
        ),
        ("analytic coverage", best),
    ] {
        let name = format!("d-edges-{}", variant.replace(' ', ""));
        let got = run.gpu(&name, &d, options, Path3d::Correct);
        run.row("D edges", variant, "edge", &got, &reference_d, &edge);
    }

    // E. Painter's order: two items through each other. The upper item covers the lower
    // one wherever they overlap, whatever their z.
    let e = [
        geometry(
            (480.0, 270.0),
            (420.0, 300.0),
            0.0,
            0.0,
            45.0,
            0.0,
            Some(800.0),
        ),
        geometry(
            (480.0, 270.0),
            (420.0, 300.0),
            0.0,
            0.0,
            -45.0,
            0.0,
            Some(800.0),
        ),
    ];
    let reference_e = run.reference("e-painter", &e, 8);
    let (_, _, all) = masks(&reference_e);
    let got = run.gpu("e-painter", &e, best, Path3d::Correct);
    run.row(
        "E painter's order",
        "stacking order",
        "all",
        &got,
        &reference_e,
        &all,
    );

    // CPU cost of the geometry: polygon, near clip, expansion and edge lines per item.
    let started = Instant::now();
    let rounds = 100_000;
    let mut vertices = 0usize;
    for i in 0..rounds {
        let mut g = b[0];
        g.tilt_deg = Finite32::new(-70.0 + (i % 100) as f32 * 0.1).expect("finite");
        let content = g
            .content(1024, 1024, Crop::default())
            .ok()
            .flatten()
            .expect("content");
        if let Some(poly) = polygon(&g, &content) {
            vertices += expand(&g, &content, &poly, 1.0).len() + edges(&poly).len();
        }
    }
    let per_item = started.elapsed() / rounds;
    writeln!(
        run.report,
        "\nB's four-corner projection has {behind} corner(s) behind the viewer, so ADR-0022's \
         `project()` alone draws nothing; the clipped polygon has {} vertices.\n\
         CPU geometry per item (polygon, near clip, 1 px expansion, edge lines): {per_item:?} \
         ({vertices} vertices over {rounds} items).",
        draw(&b[0], 1024, false, Path3d::Correct).map_or(0, |d| d.vertices.len()),
    )
    .expect("string");

    std::fs::write(out.join("results.md"), &run.report).expect("results");
    print!("{}", run.report);
}
