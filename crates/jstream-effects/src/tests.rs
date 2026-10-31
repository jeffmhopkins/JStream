use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use jstream_protocol::{KindCategory, ParamType, Settings};
use serde_json::{Value, json};

use crate::{FieldType, KindChange, Pack, PackError, PackWatcher};

const PRELUDE: &str = jstream_render::FILTER_PRELUDE;

const MANIFEST: &str = r##"
[pack]
id = "retro"
name = "Retro"
description = "Old screens"

[[effect]]
id = "scanlines"
name = "Scanlines"
version = 2
shader = "scanlines.wgsl"
renames = { amount = "strength" }

[[effect.param]]
id = "strength"
label = "Strength"
type = "float"
min = 0.0
max = 1.0
default = 0.5
unit = "%"

[[effect.param]]
id = "tint"
label = "Tint"
type = "color"
alpha = true
default = "#ff000080"

[[effect.param]]
id = "lines"
label = "Lines"
type = "int"
min = 1
max = 2160
default = 240

[[effect.param]]
id = "mode"
label = "Mode"
type = "enum"
default = "dark"
options = [{ value = "dark", label = "Dark" }, { value = "bright", label = "Bright" }]

[[effect.param]]
id = "flicker"
label = "Flicker"
type = "bool"
default = true
advanced = true

[[effect]]
id = "plain"
name = "Plain"
version = 1
shader = "shaders/plain.wgsl"
"##;

const SCANLINES: &str = r#"
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    let row = floor(in.uv.y * f32(p.lines));
    let dark = select(1.0, 1.0 - p.strength, row % 2.0 == 0.0);
    return vec4<f32>(c.rgb * dark * p.tint.rgb, c.a);
}
"#;

const PLAIN: &str = r#"
var<private> scale: f32 = 1.0;

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return src(in.uv) * scale;
}
"#;

/// A fresh, empty folder under the system temp folder, removed on drop.
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("jstream-effects-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create dir");
    }
    std::fs::write(path, text).expect("write file");
    // Each write looks newer than the last, whatever the file system's resolution.
    let later = SystemTime::now() + Duration::from_secs(next_tick());
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open file");
    file.set_modified(later).expect("set mtime");
}

fn next_tick() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TICK: AtomicU64 = AtomicU64::new(10);
    TICK.fetch_add(10, Ordering::Relaxed)
}

/// Writes the retro pack into `dir`, with `manifest` and `scanlines` swapped in.
fn retro(dir: &Path, manifest: &str, scanlines: &str) {
    write(&dir.join("effect.toml"), manifest);
    write(&dir.join("scanlines.wgsl"), scanlines);
    write(&dir.join("shaders/plain.wgsl"), PLAIN);
}

fn load(name: &str, manifest: &str, scanlines: &str) -> Result<Pack, PackError> {
    let temp = Temp::new(name);
    retro(&temp.0, manifest, scanlines);
    Pack::load(&temp.0, PRELUDE)
}

fn settings(pairs: &[(&str, Value)]) -> Settings {
    Settings(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn f32_at(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

/// The error message of a pack that must fail.
fn error(name: &str, manifest: &str, scanlines: &str) -> String {
    match load(name, manifest, scanlines) {
        Ok(_) => panic!("{name}: the pack loaded"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_pack_becomes_video_filter_kinds() {
    let pack = load("kinds", MANIFEST, SCANLINES).expect("pack loads");
    assert_eq!(pack.id(), "retro");
    assert_eq!(pack.name(), "Retro");
    assert_eq!(pack.description(), Some("Old screens"));
    let ids: Vec<&str> = pack.effects().iter().map(|e| e.kind_id()).collect();
    assert_eq!(ids, ["retro/scanlines", "retro/plain"]);

    let scan = pack.effect("retro/scanlines").expect("scanlines");
    let d = scan.descriptor();
    assert_eq!(d.category, KindCategory::VideoFilter);
    assert_eq!((d.version, d.video, d.audio), (2, true, false));
    assert_eq!(d.name, "Scanlines");
    let params: Vec<&str> = d.params.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(params, ["strength", "tint", "lines", "mode", "flicker"]);
    assert!(d.params.iter().all(|p| p.live));
    assert!(d.param("flicker").is_some_and(|p| p.advanced));
    assert_eq!(
        d.param("strength").map(|p| &p.ty),
        Some(&ParamType::Float {
            min: Some(0.0),
            max: Some(1.0),
            step: None,
            unit: Some("%".into()),
        })
    );
    // Every default passes the protocol's own check.
    for p in &d.params {
        assert_eq!(p.check(&p.default), Ok(()), "{}", p.id);
    }
    assert!(scan.wgsl().contains("var<uniform> p: Params;"));
    assert!(scan.wgsl().contains("fn fs("));

    // An effect without parameters declares no `Params`.
    let plain = pack.effect("retro/plain").expect("plain");
    assert!(!plain.wgsl().contains("Params"));
    assert_eq!(plain.layout().size(), 16);
}

#[test]
fn the_layout_matches_what_naga_computes() {
    let pack = load("layout", MANIFEST, SCANLINES).expect("pack loads");
    let scan = &pack.effects()[0];
    let module =
        naga::front::wgsl::parse_str(&format!("{PRELUDE}\n{}", scan.wgsl())).expect("parses");
    let (_, params) = module
        .types
        .iter()
        .find(|(_, t)| t.name.as_deref() == Some("Params"))
        .expect("Params");
    let naga::TypeInner::Struct { members, span } = &params.inner else {
        panic!("Params is a struct");
    };
    let ours: Vec<(&str, FieldType, usize)> = scan.layout().fields().collect();
    assert_eq!(members.len(), ours.len());
    for (member, (id, _, at)) in members.iter().zip(&ours) {
        assert_eq!(member.name.as_deref(), Some(*id));
        assert_eq!(member.offset as usize, *at, "{id}");
    }
    assert!(scan.layout().size() >= *span as usize);
    assert_eq!(scan.layout().size() % 16, 0);
    // f32 at 0, vec4 at 16, i32 at 32, u32 at 36 and 40.
    let offsets: Vec<usize> = ours.iter().map(|f| f.2).collect();
    assert_eq!(offsets, [0, 16, 32, 36, 40]);
}

#[test]
fn settings_become_uniform_bytes() {
    let pack = load("uniforms", MANIFEST, SCANLINES).expect("pack loads");
    let scan = &pack.effects()[0];

    let defaults = scan.uniforms(&Settings::default());
    assert_eq!(defaults.len(), 48);
    assert_eq!(f32_at(&defaults, 0), 0.5);
    // `#ff000080`: linear red 1, alpha 128/255, straight.
    assert_eq!(f32_at(&defaults, 16), 1.0);
    assert_eq!(f32_at(&defaults, 20), 0.0);
    assert!((f32_at(&defaults, 28) - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(
        i32::from_le_bytes(defaults[32..36].try_into().expect("4")),
        240
    );
    assert_eq!(u32_at(&defaults, 36), 0);
    assert_eq!(u32_at(&defaults, 40), 1);

    let set = scan.uniforms(&settings(&[
        ("strength", json!(0.25)),
        ("tint", json!("#808080")),
        ("lines", json!(1080)),
        ("mode", json!("bright")),
        ("flicker", json!(false)),
    ]));
    assert_eq!(f32_at(&set, 0), 0.25);
    // sRGB 0x80 is about 0.2158 in linear light; no alpha byte reads as opaque.
    assert!((f32_at(&set, 16) - 0.2158).abs() < 1e-3);
    assert_eq!(f32_at(&set, 28), 1.0);
    assert_eq!(i32::from_le_bytes(set[32..36].try_into().expect("4")), 1080);
    assert_eq!(u32_at(&set, 36), 1);
    assert_eq!(u32_at(&set, 40), 0);

    // Out of range or mistyped reads as the default.
    let bad = scan.uniforms(&settings(&[
        ("strength", json!(7.0)),
        ("mode", json!("neon")),
        ("lines", json!("many")),
    ]));
    assert_eq!(f32_at(&bad, 0), 0.5);
    assert_eq!(u32_at(&bad, 36), 0);
    assert_eq!(i32::from_le_bytes(bad[32..36].try_into().expect("4")), 240);
}

#[test]
fn renames_migrate_older_settings() {
    let pack = load("migrate", MANIFEST, SCANLINES).expect("pack loads");
    let scan = &pack.effects()[0];
    let old = settings(&[("amount", json!(0.75)), ("lines", json!(10))]);
    let migrated = scan.migrate(&old, 1).expect("older version migrates");
    assert_eq!(migrated.0.get("strength"), Some(&json!(0.75)));
    assert!(!migrated.0.contains_key("amount"));
    assert_eq!(migrated.0.get("lines"), Some(&json!(10)));
    // Settings of the current version are left alone, and a newer version's are not
    // migrated at all.
    assert_eq!(scan.migrate(&old, 2), Some(old.clone()));
    assert_eq!(scan.migrate(&old, 3), None);
}

#[test]
fn manifest_rules_name_the_field() {
    let cases: &[(&str, &str, &str, &str)] = &[
        ("bad-id", r#"id = "retro""#, r#"id = "Retro""#, "pack.id"),
        ("clap", r#"id = "retro""#, r#"id = "clap""#, "reserved"),
        (
            "dup-effect",
            r#"id = "plain""#,
            r#"id = "scanlines""#,
            "another effect has this ID",
        ),
        (
            "dup-param",
            r#"id = "lines""#,
            r#"id = "strength""#,
            "another parameter has this ID",
        ),
        (
            "default-range",
            "default = 0.5",
            "default = 1.5",
            "effect.scanlines.param.strength.default: the default must be from 0 to 1",
        ),
        (
            "int-bounds",
            "min = 1\nmax = 2160",
            "min = 1",
            "both `min` and `max`",
        ),
        (
            "int-i32",
            "max = 2160",
            "max = 9000000000",
            "fit a 32-bit integer",
        ),
        (
            "string-type",
            r#"type = "bool""#,
            r#"type = "string""#,
            "unknown variant",
        ),
        (
            "unknown-field",
            "advanced = true",
            "advanced = true\nhidden = true",
            "unknown field",
        ),
        (
            "rename-target",
            r#"renames = { amount = "strength" }"#,
            r#"renames = { amount = "gain" }"#,
            "`gain` is not a declared parameter",
        ),
        (
            "rename-declared",
            r#"renames = { amount = "strength" }"#,
            r#"renames = { lines = "strength" }"#,
            "must not still be declared",
        ),
        (
            "rename-v1",
            "version = 2",
            "version = 1",
            "a rename comes with a version above 1",
        ),
        (
            "escape",
            r#"shader = "scanlines.wgsl""#,
            r#"shader = "../scanlines.wgsl""#,
            "`..`",
        ),
        (
            "not-wgsl",
            r#"shader = "scanlines.wgsl""#,
            r#"shader = "scanlines.glsl""#,
            "`.wgsl`",
        ),
        (
            "enum-default",
            r#"default = "dark""#,
            r#"default = "neon""#,
            "must be one of dark, bright",
        ),
    ];
    for (name, from, to, expect) in cases {
        assert!(MANIFEST.contains(from), "{name}: fixture lacks {from}");
        let manifest = MANIFEST.replacen(from, to, 1);
        let message = error(name, &manifest, SCANLINES);
        assert!(message.contains(expect), "{name}: {message}");
    }
}

#[test]
fn shaders_are_checked_against_the_contract() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "syntax",
            "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n    return src(in.uv)\n}\n",
            "line 4:",
        ),
        (
            "no-fs",
            "@fragment\nfn main(in: VsOut) -> @location(0) vec4<f32> { return src(in.uv); }\n",
            "an effect defines only",
        ),
        (
            "missing",
            "fn helper() -> f32 { return 1.0; }\n",
            "defines no `@fragment fn fs",
        ),
        (
            "binding",
            "@group(1) @binding(0) var extra: texture_2d<f32>;\n\
             @fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> { return src(in.uv); }\n",
            "declares `extra` in the Handle address space",
        ),
        (
            "return",
            "@fragment\nfn fs(in: VsOut) -> @location(0) vec2<f32> { return in.uv; }\n",
            "must return `@location(0) vec4<f32>`",
        ),
        (
            "override",
            "override k: f32;\n@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> { return src(in.uv) * k; }\n",
            "declares the override `k`",
        ),
        (
            "undefined",
            "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> { return p.missing; }\n",
            "line 2:",
        ),
        // #349: memory a fragment shader has no business holding.
        (
            "huge-local",
            "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n\
             var a: array<f32, 1000000>;\n    a[0] = 1.0;\n    return src(in.uv) * a[0];\n}\n",
            "uses a 4000000-byte value",
        ),
        (
            "huge-constructed",
            "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n\
             let a = array<f32, 5000>();\n    return src(in.uv) * a[1];\n}\n",
            "uses a 20000-byte value",
        ),
        (
            "private-total",
            "var<private> a: array<vec4<f32>, 600>;\nvar<private> b: array<vec4<f32>, 600>;\n\
             @fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> { return src(in.uv) + a[0] + b[0]; }\n",
            "declares 19200 bytes of `var<private>` globals",
        ),
        (
            "locals-total",
            "@fragment\nfn fs(in: VsOut) -> @location(0) vec4<f32> {\n\
             var a: array<vec4<f32>, 600>;\n    var b: array<vec4<f32>, 600>;\n\
             return src(in.uv) + a[0] + b[0];\n}\n",
            "`fs` declares 19200 bytes of local variables",
        ),
    ];
    for (name, wgsl, expect) in cases {
        let message = error(name, MANIFEST, wgsl);
        assert!(message.contains("scanlines.wgsl"), "{name}: {message}");
        assert!(message.contains(expect), "{name}: {message}");
    }

    // A parameter ID WGSL reserves fails in the generated header, and says so.
    let manifest = MANIFEST.replacen(r#"id = "flicker""#, r#"id = "loop""#, 1);
    let message = error("reserved", &manifest, SCANLINES);
    assert!(message.contains("generated `Params`"), "{message}");

    // A lookup table within the limit loads.
    let table = format!("const LUT = array<vec4<f32>, 256>();\n{SCANLINES}");
    load("lut-ok", MANIFEST, &table).expect("a 4 KiB table is within MAX_VALUE_BYTES");
}

#[test]
fn oversized_files_are_refused() {
    let big = format!("// {}\n{SCANLINES}", "x".repeat(300 * 1024));
    let message = error("big", MANIFEST, &big);
    assert!(
        message.contains("larger than the 262144-byte limit"),
        "{message}"
    );
}

#[test]
fn the_watcher_hot_reloads_packs() {
    let temp = Temp::new("watch");
    let mut watcher = PackWatcher::new(&temp.0, PRELUDE);

    // Nothing yet, and a missing root is just empty.
    let changes = watcher.poll();
    assert!(changes.kinds.is_empty() && changes.errors.is_empty());

    let dir = temp.0.join("retro");
    retro(&dir, MANIFEST, SCANLINES);
    let changes = watcher.poll();
    assert!(changes.errors.is_empty(), "{:?}", changes.errors);
    assert_eq!(
        changes.kinds,
        [
            KindChange::Added("retro/plain".into()),
            KindChange::Added("retro/scanlines".into()),
        ]
    );
    assert!(watcher.effect("retro/scanlines").is_some());

    // No change, no report.
    let changes = watcher.poll();
    assert!(changes.kinds.is_empty() && changes.errors.is_empty());

    // An edited shader changes one kind.
    write(
        &dir.join("scanlines.wgsl"),
        &SCANLINES.replace("1.0 - p.strength", "1.0 - 0.5 * p.strength"),
    );
    assert_eq!(
        watcher.poll().kinds,
        [KindChange::Changed("retro/scanlines".into())]
    );

    // A broken save is reported once and keeps the last good version.
    write(&dir.join("scanlines.wgsl"), "@fragment fn fs(");
    let changes = watcher.poll();
    assert!(changes.kinds.is_empty());
    assert_eq!(changes.errors.len(), 1);
    assert!(watcher.effect("retro/scanlines").is_some());
    assert!(watcher.poll().errors.is_empty());

    // Fixing it reloads it.
    write(&dir.join("scanlines.wgsl"), SCANLINES);
    assert_eq!(
        watcher.poll().kinds,
        [KindChange::Changed("retro/scanlines".into())]
    );

    // A shader the manifest names before it exists is watched, and loads once written.
    std::fs::remove_file(dir.join("scanlines.wgsl")).expect("remove shader");
    assert_eq!(watcher.poll().errors.len(), 1);
    assert!(watcher.poll().errors.is_empty());
    write(&dir.join("scanlines.wgsl"), SCANLINES);
    let changes = watcher.poll();
    assert!(changes.errors.is_empty() && changes.kinds.is_empty());

    // Files the manifest doesn't name are not watched.
    write(&dir.join("notes.txt"), "draft");
    let changes = watcher.poll();
    assert!(changes.errors.is_empty() && changes.kinds.is_empty());

    // An effect dropped from the manifest is removed.
    let without_plain = &MANIFEST[..MANIFEST.find("[[effect]]\nid = \"plain\"").expect("plain")];
    write(&dir.join("effect.toml"), without_plain);
    assert_eq!(
        watcher.poll().kinds,
        [KindChange::Removed("retro/plain".into())]
    );

    // A second folder with the same pack ID is refused.
    let copy = temp.0.join("retro-copy");
    retro(&copy, MANIFEST, SCANLINES);
    let changes = watcher.poll();
    assert!(changes.kinds.is_empty());
    assert!(
        matches!(&changes.errors[..], [PackError::DuplicatePack { id, .. }] if id == "retro"),
        "{:?}",
        changes.errors
    );
    assert!(watcher.poll().errors.is_empty());

    // Removing the first folder frees the ID, and the copy loads in the same poll.
    std::fs::remove_dir_all(&dir).expect("remove pack");
    let changes = watcher.poll();
    assert!(changes.errors.is_empty(), "{:?}", changes.errors);
    assert_eq!(
        changes.kinds,
        [
            KindChange::Removed("retro/scanlines".into()),
            KindChange::Added("retro/plain".into()),
            KindChange::Added("retro/scanlines".into()),
        ]
    );
    assert!(watcher.packs().all(|p| p.dir() == copy));

    // A pack that changes its ID frees the old one too.
    let third = temp.0.join("retro-third");
    retro(&third, MANIFEST, SCANLINES);
    assert_eq!(watcher.poll().errors.len(), 1);
    write(
        &copy.join("effect.toml"),
        &MANIFEST.replacen(r#"id = "retro""#, r#"id = "neo""#, 1),
    );
    let changes = watcher.poll();
    assert!(changes.errors.is_empty(), "{:?}", changes.errors);
    // neo/* added; retro/* removed from the copy, then added from the third folder.
    assert_eq!(changes.kinds.len(), 6, "{:?}", changes.kinds);
    assert!(
        changes
            .kinds
            .contains(&KindChange::Added("neo/plain".into()))
    );
    assert!(
        changes
            .kinds
            .contains(&KindChange::Added("retro/plain".into()))
    );
    assert_eq!(watcher.packs().count(), 2);

    // Removing the folders removes their kinds.
    std::fs::remove_dir_all(&copy).expect("remove copy");
    std::fs::remove_dir_all(&third).expect("remove third");
    assert_eq!(watcher.poll().kinds.len(), 4);
    assert_eq!(watcher.packs().count(), 0);
}

/// The test compositor, or `None` (a skip) where no adapter exists outside CI.
fn compositor() -> Option<jstream_render::Compositor> {
    match jstream_render::Compositor::open(jstream_render::OpenOptions::headless()) {
        Ok(compositor) => Some(compositor),
        Err(err) if err.is_no_adapter() && std::env::var_os("CI").is_none() => None,
        Err(err) => panic!("opening the compositor failed: {err}"),
    }
}

/// `image` placed 1:1 on a transparent canvas of its size, read back as sRGB RGBA8.
fn read(compositor: &mut jstream_render::Compositor, image: &jstream_render::Image) -> Vec<u8> {
    use jstream_render::{Color, Crop, Item, Placement, Scene};
    let items = [Item {
        image,
        placement: Placement::IDENTITY,
        crop: Crop::NONE,
        opacity: 1.0,
        blend: jstream_render::BlendMode::Normal,
        visible: true,
    }];
    let scene = Scene {
        canvas: jstream_media::FrameSize::new(image.width(), image.height()).expect("non-zero"),
        background: Color::TRANSPARENT,
        items: &items,
    };
    let output = compositor.render(&scene, true).expect("render");
    let snapshot = output.pixels().expect("readback requested");
    (0..image.height())
        .flat_map(|y| (0..image.width()).map(move |x| (x, y)))
        .flat_map(|(x, y)| snapshot.pixel(x, y).expect("inside"))
        .collect()
}

fn near(got: &[u8], expected: &[u8], tol: u8) {
    assert_eq!(got.len(), expected.len());
    for (g, e) in got.iter().zip(expected) {
        assert!(g.abs_diff(*e) <= tol, "got {got:?}, expected {expected:?}");
    }
}

#[test]
fn an_effect_draws_through_the_render_harness_and_reloads() {
    let Some(mut compositor) = compositor() else {
        return;
    };
    let temp = Temp::new("draw");
    retro(&temp.0, MANIFEST, SCANLINES);
    let pack = Pack::load(&temp.0, PRELUDE).expect("pack loads");
    let scan = pack.effect("retro/scanlines").expect("scanlines");
    let input = compositor
        .upload_rgba8(1, 2, &[255, 255, 255, 255, 255, 255, 255, 255])
        .expect("upload");
    let clock = jstream_render::FilterClock { seconds: 0.0 };

    // One line, half strength, a white tint: linear 0.5 is sRGB 188.
    let mut filter = scan.video_filter(&settings(&[
        ("strength", json!(0.5)),
        ("tint", json!("#ffffff")),
        ("lines", json!(1)),
    ]));
    let out = compositor
        .apply_filter(&mut filter, &input, clock)
        .expect("apply");
    near(
        &read(&mut compositor, &out),
        &[188, 188, 188, 255, 188, 188, 188, 255],
        2,
    );

    // Settings change in place: the default red, half-transparent tint.
    filter.set_settings(&settings(&[("strength", json!(0.0)), ("lines", json!(1))]));
    let out = compositor
        .apply_filter(&mut filter, &input, clock)
        .expect("apply");
    near(
        &read(&mut compositor, &out),
        &[255, 0, 0, 255, 255, 0, 0, 255],
        2,
    );

    // A reload with new WGSL under the same kind ID draws the new shader.
    write(
        &temp.0.join("scanlines.wgsl"),
        &SCANLINES.replace(
            "return vec4<f32>(c.rgb * dark * p.tint.rgb, c.a);",
            "return vec4<f32>(0.0, 0.0, c.a * dark, c.a);",
        ),
    );
    let reloaded = Pack::load(&temp.0, PRELUDE).expect("pack reloads");
    let mut filter = reloaded.effects()[0].video_filter(&Settings::default());
    let out = compositor
        .apply_filter(&mut filter, &input, clock)
        .expect("apply");
    // The defaults: 240 lines put both pixels on even (darkened) rows.
    near(
        &read(&mut compositor, &out),
        &[0, 0, 188, 255, 0, 0, 188, 255],
        2,
    );
}
