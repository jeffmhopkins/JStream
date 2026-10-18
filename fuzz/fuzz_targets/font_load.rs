//! The `text` source's font loading: a TrueType or OpenType font, or a collection, from a
//! shared collection.
//!
//! The input is the font file. It goes through the real path: written to a folder, read as
//! a `collection` asset, parsed (`skrifa`) and drawn (`ab_glyph_rasterizer`) by
//! `Registry::create`. Nothing may panic, and limits are errors, not aborts. A font that
//! loads is drawn again by a live update, with wrapping and an outline.

#![no_main]

use jstream_media::{MediaTime, Timebase};
use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_sources::{AssetRoots, Context, Limits, Registry};
use libfuzzer_sys::fuzz_target;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::OnceLock;

/// The file's name inside the collection folder.
const FILE: &str = "input.ttf";

/// Text that reaches simple and composite glyphs, missing glyphs, spaces and a newline.
const TEXT: &str = "Hello, World! 0123456789\nÅçéñ ß — “quotes” ﬁ 漢字 🙂";

/// A folder for this process, so parallel jobs (`-jobs`, `-fork`) don't share the file.
fn folder() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("jstream-fuzz-font-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp folder");
        dir
    })
}

fuzz_target!(|data: &[u8]| {
    let dir = folder();
    std::fs::write(dir.join(FILE), data).expect("the input is written");
    let ctx = Context {
        assets: AssetRoots {
            collection: Some(dir.clone()),
            app: None,
        },
        // Smaller than the defaults, so a font with huge glyph bounds that the limits
        // allow can't push the run past libFuzzer's memory limit; the checks are the
        // same code.
        limits: Limits {
            max_side: 4096,
            max_decoded_bytes: 256 << 20,
            ..Limits::default()
        },
        ..Context::default()
    };
    let registry = Registry::builtin();
    let kind = registry.descriptor("text").expect("the text kind");
    // The kind's defaults, as the graph would store them, with the font and text set.
    let mut settings: Settings = kind
        .params
        .iter()
        .map(|p| (p.id.clone(), p.default.clone()))
        .collect();
    let font = Reference::Asset {
        root: AssetRoot::Collection,
        path: FILE.into(),
    };
    settings.insert("font", font.to_value());
    settings.insert("text", Value::from(TEXT));
    settings.insert("size", json!(32.0));
    let Ok(mut source) = registry.create("text", &settings, &ctx) else {
        return;
    };
    source
        .frame(MediaTime::new(0, Timebase::MILLIS))
        .expect("a text source draws");

    // Live style changes draw again with the same font.
    settings.insert("wrap_width", json!(120));
    settings.insert("outline_width", json!(3));
    settings.insert("align", json!("center"));
    let _ = source.update(&settings);
});
