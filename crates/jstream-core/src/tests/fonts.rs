//! `font.list` (ADR-0092): the fonts in the font folders, read once and kept, with each
//! one's family, style and absolute path.

use super::{connected, query};
use crate::Engine;
use jstream_protocol::{FontInfo, Query, QueryResult};
use std::fs;

const REGULAR: &[u8] =
    include_bytes!("../../../jstream-sources/assets/fonts/Roboto-Regular-subset.ttf");

#[test]
fn font_list_reports_the_folders_fonts_and_keeps_them() {
    let dir = std::env::temp_dir().join(format!("jstream-core-fonts-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a temp folder");
    fs::write(dir.join("Roboto.ttf"), REGULAR).expect("a font");
    let engine = Engine::new().with_font_dirs(vec![dir.clone()]);
    let control = connected(&engine);

    let list = || match query(&control, Query::FontList) {
        Ok(QueryResult::FontList { fonts }) => fonts,
        other => panic!("a font.list result, not {other:?}"),
    };
    let expected = vec![FontInfo {
        remote_family: "Roboto".into(),
        remote_style: "Regular".into(),
        path: dir.join("Roboto.ttf").to_string_lossy().into_owned(),
    }];
    assert_eq!(list(), expected);

    // Read once: a font added later shows after a restart, not now.
    fs::write(dir.join("Again.ttf"), REGULAR).expect("a font");
    assert_eq!(list(), expected);
    let _ = fs::remove_dir_all(&dir);
}

/// #1051: a font whose path isn't UTF-8 is left out, since a lossy copy of its path
/// would name another file. The folder's other fonts are still listed.
#[cfg(unix)]
#[test]
fn font_list_leaves_out_a_path_that_is_not_utf8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = std::env::temp_dir().join(format!("jstream-core-fonts-utf8-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a temp folder");
    let bad = dir.join(OsStr::from_bytes(b"Bad\xff.ttf"));
    if fs::write(&bad, REGULAR).is_err() {
        // A file system that only takes UTF-8 names (macOS's APFS) can't hold one.
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    fs::write(dir.join("Roboto.ttf"), REGULAR).expect("a font");
    // Both are fonts the folder holds: only the path keeps one out of `font.list`.
    assert_eq!(
        jstream_sources::installed_fonts(std::slice::from_ref(&dir)).len(),
        2
    );

    let engine = Engine::new().with_font_dirs(vec![dir.clone()]);
    let control = connected(&engine);
    let fonts = match query(&control, Query::FontList) {
        Ok(QueryResult::FontList { fonts }) => fonts,
        other => panic!("a font.list result, not {other:?}"),
    };
    assert_eq!(
        fonts,
        vec![FontInfo {
            remote_family: "Roboto".into(),
            remote_style: "Regular".into(),
            path: dir.join("Roboto.ttf").to_string_lossy().into_owned(),
        }]
    );
    let _ = fs::remove_dir_all(&dir);
}
