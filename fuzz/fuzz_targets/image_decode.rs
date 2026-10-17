//! The `image` source's decoders: a PNG, JPEG, WebP or GIF file from a shared collection.
//!
//! The input is the file. It goes through the real path: written to a folder, read as a
//! `collection` asset and decoded by `Registry::create`, which picks the decoder by the
//! file's first bytes. Nothing may panic, and limits are errors, not aborts. A source
//! that is created draws a frame at several times, and each change it announces is later
//! than the time it was asked at.

#![no_main]

use jstream_media::{MediaTime, Timebase};
use jstream_protocol::{AssetRoot, Reference, Settings};
use jstream_sources::{AssetRoots, Context, Limits, Registry};
use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The file's name inside the collection folder.
const FILE: &str = "input.img";

/// A folder for this process, so parallel jobs (`-jobs`, `-fork`) don't share the file.
fn folder() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("jstream-fuzz-image-{}", std::process::id()));
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
        // Smaller than the defaults, so a crafted header that the limits allow can't push
        // the run past libFuzzer's memory limit; the checks are the same code.
        limits: Limits {
            max_side: 4096,
            max_decoded_bytes: 256 << 20,
            ..Limits::default()
        },
        ..Context::default()
    };
    let file = Reference::Asset {
        root: AssetRoot::Collection,
        path: FILE.into(),
    };
    let settings: Settings = [("file", file.to_value())].into_iter().collect();
    let Ok(mut source) = Registry::builtin().create("image", &settings, &ctx) else {
        return;
    };
    for ms in [0, 1, 99, 100, 1_000, 60_000, i64::MAX / 2] {
        let at = MediaTime::new(ms, Timebase::MILLIS);
        source.frame(at).expect("a decoded image draws");
        if let Some(next) = source.next_change(at) {
            assert!(next > at, "the change after {at:?} is at {next:?}");
        }
    }
});
