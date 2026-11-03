//! The fonts installed on this machine, for `font.list` (ADR-0092): every font file in
//! the OS's font folders that parses, with the family and style its name table gives,
//! so a UI can offer them for a text source's `font` setting, which names one by its
//! absolute path.
//!
//! The walk is bounded: at most [`MAX_DEPTH`] folders deep and [`MAX_FONTS`] files, and
//! of each file it reads only the header, the table directory and the name table (at
//! most 1 MiB). It follows a link to a file (font folders are full of them) but never a
//! link to a folder, so it can't loop.

use skrifa::raw::tables::name::{Name, NameId, NameRecord};
use skrifa::raw::{FontData, FontRead};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// How many folders below a font folder the walk goes.
pub const MAX_DEPTH: usize = 8;
/// How many font files the walk reads, in all.
pub const MAX_FONTS: usize = 8192;
/// The longest family or style name kept, in characters; a longer one is cut.
pub const MAX_NAME_CHARS: usize = 128;

/// One installed font: a font file, or a collection's first font, which is the one the
/// text source draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledFont {
    /// Its family, the typographic family when the font names one.
    pub family: String,
    /// Its style within the family, `Regular` when the font names none.
    pub style: String,
    /// The file's path.
    pub path: PathBuf,
}

/// The OS's font folders, the system's and the user's, whether or not they exist.
pub fn system_font_dirs() -> Vec<PathBuf> {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut dirs = Vec::new();
    if cfg!(target_os = "windows") {
        dirs.push(
            env("WINDIR")
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
                .join("Fonts"),
        );
        if let Some(local) = env("LOCALAPPDATA") {
            dirs.push(local.join(r"Microsoft\Windows\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
        if let Some(home) = env("HOME") {
            dirs.push(home.join("Library/Fonts"));
        }
    } else {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        let data = env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")));
        if let Some(data) = data {
            dirs.push(data.join("fonts"));
        }
        if let Some(home) = env("HOME") {
            dirs.push(home.join(".fonts"));
        }
    }
    dirs
}

/// The fonts in `dirs` and the folders below them, ordered by family (ignoring case),
/// then style, then path, each file once. A file that isn't a font, can't be read or
/// names no family is left out.
pub fn installed_fonts(dirs: &[PathBuf]) -> Vec<InstalledFont> {
    let mut files = Vec::new();
    for dir in dirs {
        walk(dir, 0, &mut files);
    }
    files.sort();
    files.dedup();
    let mut fonts: Vec<InstalledFont> = files.into_iter().filter_map(|path| read(&path)).collect();
    fonts.sort_by(|a, b| {
        (a.family.to_lowercase(), &a.style, &a.path).cmp(&(
            b.family.to_lowercase(),
            &b.style,
            &b.path,
        ))
    });
    fonts
}

/// Adds the font files in `dir` and below it to `files`, up to [`MAX_FONTS`].
fn walk(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for path in entries {
        if files.len() >= MAX_FONTS {
            return;
        }
        let Ok(own) = fs::symlink_metadata(&path) else {
            continue;
        };
        if own.is_dir() {
            if depth + 1 < MAX_DEPTH {
                walk(&path, depth + 1, files);
            }
        } else if is_font_name(&path) && fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            files.push(path);
        }
    }
}

/// Whether the file's extension is one the text source's `font` accepts.
fn is_font_name(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["ttf", "otf", "ttc"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    })
}

/// The largest name table read; a font whose name table is larger is left out.
const MAX_NAME_TABLE: u32 = 1 << 20;

/// The font at `path`, or `None` if it can't be read or names no family. Only the
/// header, the table directory and the name table are read, not the whole file, so the
/// walk stays quick over large fonts.
fn read(path: &Path) -> Option<InstalledFont> {
    let table = name_table(path)?;
    let name = Name::read(FontData::new(&table)).ok()?;
    let text = |ids: &[NameId]| {
        ids.iter().find_map(|id| {
            let text = clean(&best_string(&name, *id)?);
            (!text.is_empty()).then_some(text)
        })
    };
    let family = text(&[NameId::TYPOGRAPHIC_FAMILY_NAME, NameId::FAMILY_NAME])?;
    let style = text(&[NameId::TYPOGRAPHIC_SUBFAMILY_NAME, NameId::SUBFAMILY_NAME])
        .unwrap_or_else(|| "Regular".into());
    Some(InstalledFont {
        family,
        style,
        path: path.to_path_buf(),
    })
}

/// The bytes of the name table of the font at `path`, or of a collection's first font.
fn name_table(path: &Path) -> Option<Vec<u8>> {
    let mut file = fs::File::open(path).ok()?;
    let mut at = |offset: u64, len: usize| -> Option<Vec<u8>> {
        let mut bytes = vec![0; len];
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut bytes).ok()?;
        Some(bytes)
    };
    let u16_at = |b: &[u8], i: usize| b.get(i..i + 2).map(|v| u16::from_be_bytes([v[0], v[1]]));
    let u32_at = |b: &[u8], i: usize| {
        b.get(i..i + 4)
            .map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]))
    };
    let head = at(0, 12)?;
    let start = if head.starts_with(b"ttcf") {
        u64::from(u32_at(&at(12, 4)?, 0)?)
    } else {
        0
    };
    let header = at(start, 12)?;
    let version = u32_at(&header, 0)?;
    if ![
        0x0001_0000,
        u32::from_be_bytes(*b"OTTO"),
        u32::from_be_bytes(*b"true"),
    ]
    .contains(&version)
    {
        return None;
    }
    let tables = usize::from(u16_at(&header, 4)?);
    let records = at(start + 12, tables * 16)?;
    let record = records.chunks_exact(16).find(|r| r.starts_with(b"name"))?;
    let (offset, len) = (u32_at(record, 8)?, u32_at(record, 12)?);
    if len > MAX_NAME_TABLE {
        return None;
    }
    at(u64::from(offset), usize::try_from(len).ok()?)
}

/// The text of name `id`: the US English Windows entry if there is one, else the
/// Roman Mac one, else the first that decodes.
fn best_string(name: &Name<'_>, id: NameId) -> Option<String> {
    let data = name.string_data();
    let rank = |r: &NameRecord| match (r.platform_id(), r.language_id()) {
        (3, 0x0409) => 0,
        (1, 0) => 1,
        _ => 2,
    };
    let mut records: Vec<&NameRecord> = name
        .name_record()
        .iter()
        .filter(|r| r.name_id() == id)
        .collect();
    records.sort_by_key(|r| rank(r));
    records
        .into_iter()
        .find_map(|r| Some(r.string(data).ok()?.chars().collect::<String>()))
}

/// A name as shown: control characters dropped, white space trimmed and at most
/// [`MAX_NAME_CHARS`] characters.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGULAR: &[u8] = include_bytes!("../assets/fonts/Roboto-Regular-subset.ttf");
    const BOLD: &[u8] = include_bytes!("../assets/fonts/Roboto-Bold-subset.ttf");

    /// A fresh folder of the test's own.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jstream-fonts-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a temp folder");
        dir
    }

    #[test]
    fn fonts_below_the_folders_are_listed_by_family_then_style() {
        let dir = folder("listed");
        fs::create_dir_all(dir.join("roboto/deeper")).expect("folders");
        fs::write(dir.join("roboto/deeper/B.TTF"), BOLD).expect("a font");
        fs::write(dir.join("roboto/R.ttf"), REGULAR).expect("a font");
        fs::write(dir.join("roboto/notes.txt"), b"not a font").expect("a file");
        fs::write(dir.join("broken.otf"), b"not a font either").expect("a file");
        let fonts = installed_fonts(&[dir.clone(), dir.join("missing")]);
        let shown: Vec<(&str, &str)> = fonts
            .iter()
            .map(|f| (f.family.as_str(), f.style.as_str()))
            .collect();
        assert_eq!(shown, [("Roboto", "Bold"), ("Roboto", "Regular")]);
        assert_eq!(fonts[0].path, dir.join("roboto/deeper/B.TTF"));
        // A folder named twice lists each file once.
        assert_eq!(installed_fonts(&[dir.clone(), dir.clone()]), fonts);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_walk_stops_at_its_depth() {
        let dir = folder("depth");
        let mut deep = dir.clone();
        for n in 0..MAX_DEPTH {
            deep = deep.join(n.to_string());
        }
        fs::create_dir_all(&deep).expect("folders");
        fs::write(deep.join("R.ttf"), REGULAR).expect("a font");
        fs::write(deep.parent().expect("a parent").join("R.ttf"), REGULAR).expect("a font");
        let fonts = installed_fonts(std::slice::from_ref(&dir));
        assert_eq!(fonts.len(), 1, "only the font above the limit: {fonts:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_lose_control_characters_and_are_cut() {
        assert_eq!(clean("  Ev\u{1b}il\n "), "Evil");
        assert_eq!(clean(&"x".repeat(500)).len(), MAX_NAME_CHARS);
    }

    #[test]
    fn every_platform_has_font_folders() {
        assert!(!system_font_dirs().is_empty());
    }
}
