//! Shader sources (ADR-0093): manifests, generated headers, validation, images and
//! drawing on the test adapter.

use std::path::Path;

use jstream_protocol::{KindCategory, ParamType, Settings};
use jstream_render::ProgramInput;
use serde_json::json;

use crate::tests::{Temp, compositor, near, read, settings, write};
use crate::{KindChange, Pack, PackError, PackWatcher};

const PRELUDE: &str = jstream_render::FILTER_PRELUDE;

const MANIFEST: &str = r##"
[pack]
id = "space"
name = "Space"

[[source]]
id = "warp"
name = "Warp"
version = 1
width = 64
height = 32
audio = true
common = "common.wgsl"
images = { tint = "tint.png" }

[[source.param]]
id = "speed"
label = "Speed"
type = "float"
min = 0.0
max = 10.0
default = 2.0

[[source.param]]
id = "sky"
label = "Sky"
type = "scene"

[[source.pass]]
id = "trail"
shader = "trail.wgsl"
scale = 0.5
inputs = ["trail", "tint"]

[[source.pass]]
id = "out"
shader = "out.wgsl"
inputs = ["trail", "sky", "audio"]
"##;

const COMMON: &str = "fn boost(c: vec4<f32>) -> vec4<f32> { return c * p.speed * 0.5; }\n";

/// Keeps its own last frame and the tint image's colour.
const TRAIL: &str = r#"
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let last = trail(in.uv);
    return max(last, tint_texel(vec2<i32>(0)));
}
"#;

/// Shows the trail where the sky is empty, and reads the audio helpers.
const OUT: &str = r#"
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let s = sky(in.uv);
    let level = audio_band(0) * 0.0 + audio_wave(0) * 0.0 + sys.level * 0.0;
    return boost(select(trail(in.uv), s, s.a > 0.0)) + vec4<f32>(level);
}
"#;

/// A 1x1 PNG of one straight-alpha sRGB colour.
fn png(rgba: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&rgba).expect("data");
    }
    bytes
}

fn space(dir: &Path, manifest: &str, out: &str) {
    write(&dir.join("effect.toml"), manifest);
    write(&dir.join("common.wgsl"), COMMON);
    write(&dir.join("trail.wgsl"), TRAIL);
    write(&dir.join("out.wgsl"), out);
    std::fs::write(dir.join("tint.png"), png([0, 255, 0, 255])).expect("png");
}

fn load(name: &str, manifest: &str, out: &str) -> Result<Pack, PackError> {
    let temp = Temp::new(name);
    space(&temp.0, manifest, out);
    Pack::load(&temp.0, PRELUDE)
}

fn error(name: &str, manifest: &str, out: &str) -> String {
    load(name, manifest, out)
        .expect_err("the pack must fail")
        .to_string()
}

#[test]
fn a_source_becomes_a_source_kind_with_its_settings() {
    let pack = load("kind", MANIFEST, OUT).expect("loads");
    assert!(pack.effects().is_empty());
    let source = pack.source("space/warp").expect("the source");
    let kind = source.descriptor();
    assert_eq!(kind.category, KindCategory::Source);
    assert!(kind.video && !kind.audio);
    let ids: Vec<_> = kind.params.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        ["width", "height", "audio", "audio_source", "speed", "sky"]
    );
    assert_eq!(kind.param("width").expect("width").default, json!(64));
    assert_eq!(kind.param("sky").expect("sky").ty, ParamType::Scene);
    assert!(
        kind.params
            .iter()
            .filter(|p| p.id.starts_with("audio"))
            .all(|p| !p.live)
    );
    assert_eq!(source.scene_params(), ["sky"]);
    assert!(source.hears());
    // Only `speed` has a uniform form.
    assert_eq!(source.layout().fields().len(), 1);
    assert_eq!(source.size(&Settings::default()), (64, 32));
    assert_eq!(
        source.size(&settings(&[("width", json!(128)), ("height", json!(9999))])),
        (128, 32)
    );
    let passes = source.passes();
    assert_eq!(
        passes[0].inputs,
        [ProgramInput::Pass(0), ProgramInput::Image(0)]
    );
    assert_eq!(
        passes[1].inputs,
        [
            ProgramInput::Pass(0),
            ProgramInput::Slot(0),
            ProgramInput::Audio
        ]
    );
    assert!((passes[0].scale - 0.5).abs() < f32::EPSILON);
    assert_eq!(source.files().len(), 4);
}

#[test]
fn manifest_rules_for_sources_name_the_field() {
    let cases = [
        (
            "no-pass",
            MANIFEST.replace(
                "[[source.pass]]\nid = \"trail\"",
                "[[source.unused]]\nid = \"trail\"",
            ),
            "unknown field",
        ),
        (
            "bad-input",
            MANIFEST.replace(r#"inputs = ["trail", "tint"]"#, r#"inputs = ["nope"]"#),
            "`nope` names no pass",
        ),
        (
            "twice",
            MANIFEST.replace(
                r#"inputs = ["trail", "tint"]"#,
                r#"inputs = ["tint", "tint"]"#,
            ),
            "listed twice",
        ),
        (
            "last-scale",
            MANIFEST.replace(
                "inputs = [\"trail\", \"sky\", \"audio\"]",
                "inputs = [\"trail\"]\nscale = 0.5",
            ),
            "the last pass",
        ),
        (
            "tiny-scale",
            MANIFEST.replace("scale = 0.5", "scale = 0.01"),
            "1/16 to 1",
        ),
        (
            "width",
            MANIFEST.replace("width = 64", "width = 8"),
            "16 to 4096",
        ),
        (
            "reserved",
            MANIFEST.replace(r#"id = "speed""#, r#"id = "width""#),
            "has this setting already",
        ),
        (
            "clash",
            MANIFEST.replace("images = { tint", "images = { trail"),
            "already names",
        ),
        (
            "prelude-name",
            MANIFEST.replace("id = \"trail\"\nshader", "id = \"noise\"\nshader"),
            "the shader prelude defines",
        ),
        (
            "helper-name",
            MANIFEST.replace("images = { tint", "images = { trail_wrap"),
            "a helper the name `trail` defines",
        ),
        (
            "no-audio",
            MANIFEST.replace("audio = true\n", ""),
            "`audio` names no pass",
        ),
        (
            "image-type",
            MANIFEST.replace("tint.png", "tint.gif"),
            "`.png`, `.jpg` or `.jpeg`",
        ),
        (
            "scene-in-effect",
            format!(
                "{MANIFEST}\n[[effect]]\nid = \"e\"\nname = \"E\"\nversion = 1\nshader = \"out.wgsl\"\n[[effect.param]]\nid = \"m\"\nlabel = \"M\"\ntype = \"scene\"\n"
            ),
            "an effect has no scene parameters",
        ),
        (
            "same-id",
            format!(
                "{MANIFEST}\n[[effect]]\nid = \"warp\"\nname = \"E\"\nversion = 1\nshader = \"out.wgsl\"\n"
            ),
            "another effect or source",
        ),
    ];
    for (name, manifest, expect) in cases {
        let message = error(name, &manifest, OUT);
        assert!(message.contains(expect), "{name}: {message}");
    }
}

#[test]
fn a_pass_error_names_its_file_and_line() {
    let message = error(
        "line",
        MANIFEST,
        "\n\n@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> { return nope; }\n",
    );
    assert!(message.contains("out.wgsl"), "{message}");
    assert!(message.contains("line 4"), "{message}");
    // A name the pass doesn't list is not declared for it.
    let message = error(
        "unlisted",
        MANIFEST,
        &OUT.replace("let s = sky(in.uv);", "let s = tint(in.uv);"),
    );
    assert!(
        message.contains("out.wgsl") && message.contains("tint"),
        "{message}"
    );
    // An error in the common file names that file, with its own lines.
    let temp = Temp::new("common-error");
    space(&temp.0, MANIFEST, OUT);
    write(
        &temp.0.join("common.wgsl"),
        "fn boost(c: vec4<f32>) -> vec4<f32> {\n  return nope;\n}\n",
    );
    let message = Pack::load(&temp.0, PRELUDE).expect_err("fails").to_string();
    assert!(
        message.contains("common.wgsl") && message.contains("line 2"),
        "{message}"
    );
    // A pass may not add bindings of its own.
    let message = error(
        "binding",
        MANIFEST,
        &format!("@group(0) @binding(9) var extra: texture_2d<f32>;\n{OUT}"),
    );
    assert!(message.contains("`extra`"), "{message}");
}

#[test]
fn a_bad_image_fails_the_pack() {
    let temp = Temp::new("bad-image");
    space(&temp.0, MANIFEST, OUT);
    std::fs::write(temp.0.join("tint.png"), b"not a png").expect("write");
    let message = Pack::load(&temp.0, PRELUDE).expect_err("fails").to_string();
    assert!(
        message.contains("tint.png") && message.contains("neither"),
        "{message}"
    );
}

#[test]
fn the_watcher_reports_a_source_kind_and_its_image_changing() {
    let temp = Temp::new("watch-source");
    let dir = temp.0.join("space");
    space(&dir, MANIFEST, OUT);
    let mut watcher = PackWatcher::new(&temp.0, PRELUDE);
    let changes = watcher.poll();
    assert_eq!(changes.kinds, [KindChange::Added("space/warp".into())]);
    std::fs::write(dir.join("tint.png"), png([0, 0, 255, 255])).expect("png");
    // A later modification time than the first write, whatever the clock resolution.
    let file = std::fs::File::options()
        .write(true)
        .open(dir.join("tint.png"))
        .expect("open");
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .expect("mtime");
    let changes = watcher.poll();
    assert_eq!(changes.kinds, [KindChange::Changed("space/warp".into())]);
}

/// Sets `path`'s modification time `hours` ahead, so a poll sees it changed whatever
/// the clock's resolution.
fn touch(path: &Path, hours: u64) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open");
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600 * hours))
        .expect("mtime");
}

/// A PNG of `width` by `height` opaque green pixels.
fn green(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        writer
            .write_image_data(&[0, 255, 0, 255].repeat((width * height) as usize))
            .expect("data");
    }
    bytes
}

/// A change only a pass's `scale`, or only an image's size, holds still changes the
/// kind, so the engine draws the new program.
#[test]
fn the_watcher_reports_a_scale_or_image_size_change() {
    let temp = Temp::new("watch-scale");
    let dir = temp.0.join("space");
    space(&dir, MANIFEST, OUT);
    std::fs::write(dir.join("tint.png"), green(2, 2)).expect("png");
    let mut watcher = PackWatcher::new(&temp.0, PRELUDE);
    assert_eq!(watcher.poll().kinds.len(), 1);
    let changed = [KindChange::Changed("space/warp".into())];

    write(
        &dir.join("effect.toml"),
        &MANIFEST.replace("scale = 0.5", "scale = 0.25"),
    );
    touch(&dir.join("effect.toml"), 1);
    assert_eq!(watcher.poll().kinds, changed, "only the scale changed");

    // The same 16 bytes, 4 by 1 instead of 2 by 2.
    std::fs::write(dir.join("tint.png"), green(4, 1)).expect("png");
    touch(&dir.join("tint.png"), 2);
    assert_eq!(
        watcher.poll().kinds,
        changed,
        "only the image's size changed"
    );
}

#[test]
fn a_source_draws_its_program_on_the_test_adapter() {
    let Some(mut opened) = compositor() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let pack = load("draw-source", MANIFEST, OUT).expect("loads");
    let source = pack.source("space/warp").expect("source");
    // `speed` 2 makes `boost` a no-op: the trail's green, at the source's size.
    let mut filter = source.video_filter(&settings(&[("width", json!(16)), ("height", json!(16))]));
    let stand_in = compositor.upload_rgba8(1, 1, &[0; 4]).expect("upload");
    let clock = jstream_render::FilterClock {
        seconds: 0.0,
        beat: 0.0,
    };
    let out = compositor
        .apply_filter(&mut filter, &stand_in, clock)
        .expect("draws");
    assert_eq!((out.width(), out.height()), (16, 16));
    let green: Vec<u8> = [0, 255, 0, 255].repeat(256);
    near(&read(compositor, &out), &green, 2);
    // A scene in the `sky` slot shows instead of the trail.
    let red = compositor
        .upload_rgba8(1, 1, &[255, 0, 0, 255])
        .expect("upload");
    filter.set_input(0, Some(std::sync::Arc::new(red)));
    let out = compositor
        .apply_filter(&mut filter, &stand_in, clock)
        .expect("draws");
    near(&read(compositor, &out), &[255, 0, 0, 255].repeat(256), 2);
}

/// What the custom shader editor writes for a shader source (ADR-0135,
/// `jstream-ui`'s `custom_shader::source_toml` from its `SOURCE_TEMPLATE`, with
/// `// @feedback`, `// @audio` and a `scene` setting added), as it writes it.
const EDITOR_MANIFEST: &str = r##"# Made by the Custom shader editor from the `// @` lines of shader.wgsl.
[pack]
id = "custom-1"
name = "Shader source"

[[source]]
id = "shader"
name = "Shader source"
version = 1
width = 1920
height = 1080
audio = true

[[source.param]]
id = "sky"
label = "Sky"
type = "scene"

[[source.param]]
id = "speed"
label = "Speed"
type = "float"
default = 1.0
min = 0.0
max = 4.0

[[source.param]]
id = "color_a"
label = "Colour A"
type = "color"
default = "#1b3a8cff"
alpha = true

[[source.param]]
id = "color_b"
label = "Colour B"
type = "color"
default = "#ff5c8aff"
alpha = true

[[source.pass]]
id = "previous"
shader = "shader.wgsl"
inputs = ["previous", "audio", "sky"]
"##;

/// The shader of [`EDITOR_MANIFEST`].
const EDITOR_SHADER: &str = r##"// @feedback
// @audio
// @param sky scene "Sky"
// @name Shader source
// @size 1920x1080
//
// Each `// @param` line is one setting, read in the shader as `p.<id>`:
//   // @param <id> float <min>..<max> = <default> "Label"
//   // @param <id> int <min>..<max> = <default> "Label"
//   // @param <id> bool = true "Label"            (a u32: 1 is on)
//   // @param <id> color = #rrggbbaa "Label"      (a vec4, linear light, straight alpha)
//   // @param <id> enum a|b|c = a "Label"         (a u32: the option's index)
//   // @param <id> scene "Label"                  (read as <id>(uv): that scene's picture)
// `// @size <w>x<h>` is the size it starts at (16 to 4096 px a side); Width and Height
// change it later. `// @feedback` lets it read its own last frame as previous(uv)
// (transparent at first), for trails. `// @audio` gives it Audio settings, and
// audio_band(i) (64 bands, 0 to 1), audio_wave(i), sys.level, sys.peak and sys.onset.
// Give a setting a new ID with a `// @renamed <old> <new>` line; never delete the line.
//
// Built in: sys.size is the picture's size in pixels, sys.time the seconds since it was
// last shown, sys.clock the media clock's, sys.frame the frames since, and in.uv this
// pixel, (0, 0) top left. Return premultiplied linear light.
//
// @param speed float 0..4 = 1 "Speed"
// @param color_a color = #1b3a8cff "Colour A"
// @param color_b color = #ff5c8aff "Colour B"

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let t = sys.time * p.speed;
    let wave = 0.5 + 0.5 * sin(in.uv.x * 6.2832 + t) * cos(in.uv.y * 3.1416 - t * 0.7);
    let c = mix(p.color_a, p.color_b, wave) + previous(in.uv) * 0.5 + sky(in.uv) * audio_band(0) * sys.level;
    return vec4<f32>(c.rgb * c.a, c.a);
}
"##;

#[test]
fn a_shader_source_from_the_editor_loads_and_draws() {
    let temp = Temp::new("editor-source");
    write(&temp.0.join("effect.toml"), EDITOR_MANIFEST);
    write(&temp.0.join("shader.wgsl"), EDITOR_SHADER);
    let pack = Pack::load(&temp.0, PRELUDE).expect("loads");
    let source = pack.source("custom-1/shader").expect("the source");
    let ids: Vec<_> = source
        .descriptor()
        .params
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "width",
            "height",
            "audio",
            "audio_source",
            "sky",
            "speed",
            "color_a",
            "color_b"
        ]
    );
    assert_eq!(source.scene_params(), ["sky"]);
    assert!(source.hears());
    let Some(mut opened) = compositor() else {
        return;
    };
    let compositor = &mut opened.compositor;
    let mut filter = source.video_filter(&settings(&[("width", json!(16)), ("height", json!(16))]));
    let stand_in = compositor.upload_rgba8(1, 1, &[0; 4]).expect("upload");
    let clock = jstream_render::FilterClock {
        seconds: 0.0,
        beat: 0.0,
    };
    let out = compositor
        .apply_filter(&mut filter, &stand_in, clock)
        .expect("draws");
    assert_eq!((out.width(), out.height()), (16, 16));
    // The bands between two opaque colours: every pixel opaque.
    let pixels = read(compositor, &out);
    assert!(pixels.chunks(4).all(|px| px[3] >= 250), "{pixels:?}");
}
