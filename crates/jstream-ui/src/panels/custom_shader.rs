//! Custom shaders (#1232): a video filter written as WGSL right in the Filters window.
//! Under the hood each one is an effect pack (ADR-0093, ADR-0097) with one effect,
//! `shader`, in `shader.wgsl`, written with `pack.write`, so it has a pack's validation,
//! limits and GPU-hang quarantine (ADR-0087), and shows in Add filter as a kind of its
//! own for any other item.
//!
//! The shader file is the whole of it. Its `// @name` line names the pack and its
//! effect, and each `// @param` line declares one setting, which becomes a field of
//! `p` in the shader and an ordinary filter setting: a slider, a switch, a choice or a
//! colour, animatable and settable over MCP. [`effect_toml`] makes the manifest from
//! those lines, so a person edits one file and an agent following the same
//! [`TEMPLATE`] writes a pack this editor opens.
//!
//! [`ShaderEditor`] is the editor: the shader's text with its line numbers, compiled
//! with `pack.write` a moment after each change. The engine keeps drawing the last good
//! version while the text has a problem, and the problem shows under the text with its
//! line marked. Everything else here is plain functions, tested natively.
//!
//! Docs: docs/UI-FEATURES.md#filters

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::store::{Studio, query};
use jstream_control::protocol::{
    Action, Call, PackContent, PackFile, PackFileInfo, PackInfo, Query, QueryResult,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::time::Duration;

/// How long after the last keystroke the editor compiles.
pub const COMPILE_AFTER: Duration = Duration::from_millis(400);

/// The spaces Tab puts in.
const INDENT: &str = "    ";

/// The effect's ID in every custom shader's pack, so its kind is `<pack>/shader`.
pub const EFFECT: &str = "shader";

/// The shader's file in the pack.
pub const SHADER_FILE: &str = "shader.wgsl";

/// The manifest's file in the pack.
pub const MANIFEST_FILE: &str = "effect.toml";

/// What a new custom shader's pack ID starts with, before its number.
pub const PACK_PREFIX: &str = "custom-";

/// The name a shader with no `// @name` line gets.
pub const DEFAULT_NAME: &str = "Custom shader";

/// The longest name the engine takes for a pack or an effect (its `check_name`).
const MAX_NAME: usize = 64;

/// A new custom shader's text: a tint over the input, with one setting of each common
/// type, and the built-ins a shader can use.
pub const TEMPLATE: &str = r#"// @name Custom shader
//
// Each `// @param` line is one setting, read in the shader as `p.<id>`:
//   // @param <id> float <min>..<max> = <default> "Label"
//   // @param <id> int <min>..<max> = <default> "Label"
//   // @param <id> bool = true "Label"            (a u32: 1 is on)
//   // @param <id> color = #rrggbbaa "Label"      (a vec4, linear light, straight alpha)
//   // @param <id> enum a|b|c = a "Label"         (a u32: the option's index)
// Give a setting a new ID with a `// @renamed <old> <new>` line: Save as pack… carries
// the value over, and the pack records the rename (the engine doesn't move saved
// values yet). Never delete the line: each one is a version.
//
// Built in: src(uv) is the input at uv (premultiplied linear light), texel(px) one of
// its pixels, frame.in_size and frame.out_size the sizes in pixels, frame.time the
// seconds on the media clock, and in.uv this pixel, (0, 0) top left.
//
// @param amount float 0..1 = 0.5 "Amount"
// @param tint color = #ff9a3cff "Tint"

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = src(in.uv);
    let tinted = vec4<f32>(c.rgb * p.tint.rgb, c.a);
    return mix(c, tinted, p.amount);
}
"#;

/// A setting's type and constraints, as a `// @param` line declares them.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    /// `float <min>..<max> = <default>`; the range is optional.
    Float {
        /// The default.
        default: f64,
        /// The range, when given.
        range: Option<(f64, f64)>,
    },
    /// `int <min>..<max> = <default>`; the range is optional.
    Int {
        /// The default.
        default: i64,
        /// The range, when given.
        range: Option<(i64, i64)>,
    },
    /// `bool = <true|false>`.
    Bool {
        /// The default.
        default: bool,
    },
    /// `color = #rrggbb` or `#rrggbbaa`; eight digits give it an alpha.
    Color {
        /// The default, as written.
        default: String,
    },
    /// `enum a|b|c = a`.
    Enum {
        /// The options' values, in order.
        options: Vec<String>,
        /// The default option.
        default: String,
    },
}

/// One `// @param` line.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// Its ID, a WGSL field name.
    pub id: String,
    /// Its label: the quoted text, or the ID read as words.
    pub label: String,
    /// Its type.
    pub kind: ParamKind,
}

/// What a shader's annotations declare.
#[derive(Clone, Debug, PartialEq)]
pub struct Declared {
    /// The pack's and the effect's name.
    pub name: String,
    /// The settings, in order.
    pub params: Vec<Param>,
    /// Each `// @renamed <old> <new>` line, in order: a setting saved under `old` is
    /// read as `new` (ADR-0025 › Versioning a kind).
    pub renames: Vec<(String, String)>,
}

impl Declared {
    /// The effect's version: 1, and one more for each `@renamed` line, so each rename
    /// moves the settings saved before it.
    pub fn version(&self) -> usize {
        1 + self.renames.len()
    }
}

/// A problem with one annotation, at a line of the shader (from 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineError {
    /// The line, from 1.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The text of a line comment, or `None` for a line that isn't one.
fn comment(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("//").map(str::trim)
}

/// Whether `id` is a parameter ID the engine takes: a WGSL field name of `a-z`, `0-9`
/// and single `_`, starting with a letter, at most 32 characters.
fn good_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && !id.ends_with('_')
        && !id.contains("__")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `id` read as words: `edge_width` is "Edge width".
fn words(id: &str) -> String {
    let text = id.replace('_', " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

/// Splits a trailing `"Label"` off `rest`.
fn split_label(rest: &str) -> Result<(&str, Option<String>), String> {
    let rest = rest.trim_end();
    if !rest.ends_with('"') {
        return Ok((rest, None));
    }
    let body = &rest[..rest.len() - 1];
    let Some(open) = body.rfind('"') else {
        return Err("a label is in double quotes: \"Label\"".into());
    };
    let label = body[open + 1..].trim();
    if label.is_empty() {
        return Err("a label is not empty".into());
    }
    if label.chars().count() > MAX_NAME {
        return Err(format!("a label is at most {MAX_NAME} characters"));
    }
    Ok((&body[..open], Some(label.to_owned())))
}

/// A `<min>..<max>` range.
fn range<T: std::str::FromStr + PartialOrd + Copy>(text: &str) -> Result<(T, T), String> {
    let (lo, hi) = text
        .split_once("..")
        .ok_or_else(|| "a range is `<min>..<max>`".to_owned())?;
    let num = |s: &str| {
        s.trim()
            .parse::<T>()
            .map_err(|_| format!("`{}` is not a number of this type", s.trim()))
    };
    let (lo, hi) = (num(lo)?, num(hi)?);
    if lo >= hi {
        return Err("a range's min is below its max".into());
    }
    Ok((lo, hi))
}

/// Whether `text` is `#rrggbb` or `#rrggbbaa`.
fn good_color(text: &str) -> bool {
    text.strip_prefix('#')
        .is_some_and(|hex| matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Parses the body of one `@param` line, after `@param`.
fn param(body: &str) -> Result<Param, String> {
    let (body, label) = split_label(body)?;
    let (spec, default) = body
        .split_once('=')
        .ok_or_else(|| "a setting has a default: `= <value>`".to_owned())?;
    let default = default.trim();
    if default.is_empty() {
        return Err("a setting has a default: `= <value>`".into());
    }
    let mut words_in = spec.split_whitespace();
    let id = words_in
        .next()
        .ok_or_else(|| "`@param` is followed by the setting's ID".to_owned())?;
    if !good_id(id) {
        return Err(format!(
            "`{id}` is not a setting ID: lower-case letters, digits and single `_`, \
             starting with a letter, at most 32 characters"
        ));
    }
    let ty = words_in.next().ok_or_else(|| {
        "the ID is followed by a type: float, int, bool, color or enum".to_owned()
    })?;
    let extra = words_in.next();
    if words_in.next().is_some() {
        return Err("too many words before `=`".into());
    }
    let kind = match ty {
        "float" => {
            let default: f64 = default
                .parse()
                .ok()
                .filter(|d: &f64| d.is_finite())
                .ok_or_else(|| format!("`{default}` is not a number"))?;
            let range = extra.map(range::<f64>).transpose()?;
            if let Some((lo, hi)) = range
                && !(lo..=hi).contains(&default)
            {
                return Err("the default is inside the range".into());
            }
            ParamKind::Float { default, range }
        }
        "int" => {
            let default: i64 = default
                .parse()
                .ok()
                .filter(|d| i32::try_from(*d).is_ok())
                .ok_or_else(|| format!("`{default}` is not a whole number an i32 holds"))?;
            let range = extra.map(range::<i64>).transpose()?;
            if let Some((lo, hi)) = range {
                if i32::try_from(lo).is_err() || i32::try_from(hi).is_err() {
                    return Err("an int's range is inside an i32".into());
                }
                if !(lo..=hi).contains(&default) {
                    return Err("the default is inside the range".into());
                }
            }
            ParamKind::Int { default, range }
        }
        "bool" => {
            if extra.is_some() {
                return Err("a bool has no range: `bool = true`".into());
            }
            let default = match default {
                "true" => true,
                "false" => false,
                _ => return Err("a bool's default is `true` or `false`".into()),
            };
            ParamKind::Bool { default }
        }
        "color" => {
            if extra.is_some() {
                return Err("a color has no range: `color = #rrggbbaa`".into());
            }
            if !good_color(default) {
                return Err("a color's default is `#rrggbb` or `#rrggbbaa`".into());
            }
            ParamKind::Color {
                default: default.to_ascii_lowercase(),
            }
        }
        "enum" => {
            let options: Vec<String> = extra
                .ok_or_else(|| "an enum lists its options: `enum a|b|c = a`".to_owned())?
                .split('|')
                .map(str::to_owned)
                .collect();
            for (at, o) in options.iter().enumerate() {
                if !good_id(o) {
                    return Err(format!(
                        "`{o}` is not an option: lower-case letters, digits and single `_`"
                    ));
                }
                if options[..at].contains(o) {
                    return Err(format!("`{o}` is listed twice"));
                }
            }
            if !options.iter().any(|o| o == default) {
                return Err(format!("the default `{default}` is not one of the options"));
            }
            ParamKind::Enum {
                options,
                default: default.to_owned(),
            }
        }
        other => {
            return Err(format!(
                "`{other}` is not a type: float, int, bool, color or enum"
            ));
        }
    };
    Ok(Param {
        id: id.to_owned(),
        label: label.unwrap_or_else(|| words(id)),
        kind,
    })
}

/// Reads a shader's `// @name` and `// @param` lines. Every other line, comments
/// included, is the shader's own; naga checks it when the pack loads.
pub fn declared(wgsl: &str) -> Result<Declared, LineError> {
    let mut name = None;
    let mut params: Vec<Param> = Vec::new();
    let mut renames: Vec<(String, String, usize)> = Vec::new();
    for (at, line) in wgsl.lines().enumerate() {
        let fail = |message: String| LineError {
            line: at + 1,
            message,
        };
        let Some(text) = comment(line) else {
            continue;
        };
        if let Some(rest) = text.strip_prefix("@name") {
            if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let rest = rest.trim();
            if name.is_some() {
                return Err(fail("the shader has one `@name` line".into()));
            }
            if rest.is_empty() || rest.chars().count() > MAX_NAME {
                return Err(fail(format!("a name is 1 to {MAX_NAME} characters")));
            }
            if rest.chars().any(char::is_control) {
                return Err(fail("a name has no control characters".into()));
            }
            name = Some(rest.to_owned());
        } else if let Some(rest) = text.strip_prefix("@param") {
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let p = param(rest).map_err(fail)?;
            if params.iter().any(|q| q.id == p.id) {
                return Err(fail(format!("another setting is called `{}`", p.id)));
            }
            if params.len() >= 64 {
                return Err(fail("a shader has at most 64 settings".into()));
            }
            params.push(p);
        } else if let Some(rest) = text.strip_prefix("@renamed") {
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let ids: Vec<&str> = rest.split_whitespace().collect();
            let [old, new] = ids[..] else {
                return Err(fail("a rename is `// @renamed <old id> <new id>`".into()));
            };
            if !good_id(old) || !good_id(new) || old == new {
                return Err(fail(
                    "a rename names two different setting IDs: `// @renamed <old id> <new id>`"
                        .into(),
                ));
            }
            if renames.iter().any(|(o, _, _)| o == old) {
                return Err(fail(format!("`{old}` is already renamed")));
            }
            if renames.len() >= 32 {
                return Err(fail("a shader has at most 32 renames".into()));
            }
            renames.push((old.to_owned(), new.to_owned(), at + 1));
        }
    }
    // Checked against every setting, wherever its line is.
    for (old, new, line) in &renames {
        let fail = |message: String| LineError {
            line: *line,
            message,
        };
        if params.iter().any(|p| p.id == *old) {
            return Err(fail(format!(
                "`{old}` is still a setting: a renamed ID is no longer declared"
            )));
        }
        if !params.iter().any(|p| p.id == *new) {
            return Err(fail(format!(
                "`{new}` is not a setting; after another rename, point this line at the newest ID"
            )));
        }
    }
    Ok(Declared {
        name: name.unwrap_or_else(|| DEFAULT_NAME.to_owned()),
        params,
        renames: renames.into_iter().map(|(o, n, _)| (o, n)).collect(),
    })
}

/// `text` as a TOML basic string.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A float as TOML writes it: always with a `.` or an exponent.
fn toml_float(n: f64) -> String {
    let text = format!("{n:?}");
    if text.contains(['.', 'e', 'E']) {
        text
    } else {
        format!("{text}.0")
    }
}

/// The `effect.toml` for pack `pack` with these declarations: one effect, `shader`,
/// drawn by `shader.wgsl`, at [`Declared::version`] with its renames.
pub fn effect_toml(pack: &str, declared: &Declared) -> String {
    let name = toml_string(&declared.name);
    let mut out = format!(
        "# Made by the Custom shader editor from the `// @` lines of {SHADER_FILE}.\n\
         [pack]\nid = {}\nname = {name}\n\n[[effect]]\nid = \"{EFFECT}\"\nname = {name}\n\
         version = {}\nshader = \"{SHADER_FILE}\"\n",
        toml_string(pack),
        declared.version()
    );
    if !declared.renames.is_empty() {
        let pairs: Vec<String> = declared
            .renames
            .iter()
            .map(|(old, new)| format!("{old} = {}", toml_string(new)))
            .collect();
        out.push_str(&format!("renames = {{ {} }}\n", pairs.join(", ")));
    }
    for p in &declared.params {
        out.push_str(&format!(
            "\n[[effect.param]]\nid = {}\nlabel = {}\n",
            toml_string(&p.id),
            toml_string(&p.label)
        ));
        match &p.kind {
            ParamKind::Float { default, range } => {
                out.push_str(&format!(
                    "type = \"float\"\ndefault = {}\n",
                    toml_float(*default)
                ));
                if let Some((lo, hi)) = range {
                    out.push_str(&format!(
                        "min = {}\nmax = {}\n",
                        toml_float(*lo),
                        toml_float(*hi)
                    ));
                }
            }
            ParamKind::Int { default, range } => {
                out.push_str(&format!("type = \"int\"\ndefault = {default}\n"));
                if let Some((lo, hi)) = range {
                    out.push_str(&format!("min = {lo}\nmax = {hi}\n"));
                }
            }
            ParamKind::Bool { default } => {
                out.push_str(&format!("type = \"bool\"\ndefault = {default}\n"));
            }
            ParamKind::Color { default } => {
                out.push_str(&format!(
                    "type = \"color\"\ndefault = {}\nalpha = {}\n",
                    toml_string(default),
                    default.len() == 9
                ));
            }
            ParamKind::Enum { options, default } => {
                out.push_str(&format!(
                    "type = \"enum\"\ndefault = {}\n",
                    toml_string(default)
                ));
                for o in options {
                    out.push_str(&format!(
                        "\n[[effect.param.options]]\nvalue = {}\nlabel = {}\n",
                        toml_string(o),
                        toml_string(&words(o))
                    ));
                }
            }
        }
    }
    out
}

/// The files a `pack.write` of this shader sends: the manifest its annotations make,
/// and the shader itself. An annotation's problem is the error, before anything is
/// sent.
pub fn pack_files(pack: &str, wgsl: &str) -> Result<Vec<PackFile>, LineError> {
    let declared = declared(wgsl)?;
    Ok(vec![
        PackFile {
            path: MANIFEST_FILE.to_owned(),
            content: PackContent::Text(effect_toml(pack, &declared)),
        },
        PackFile {
            path: SHADER_FILE.to_owned(),
            content: PackContent::Text(wgsl.to_owned()),
        },
    ])
}

/// The kind of the custom shader in pack `pack`.
pub fn kind_of(pack: &str) -> String {
    format!("{pack}/{EFFECT}")
}

/// The pack of a custom shader's kind: `custom-1` for `custom-1/shader`, `None` for a
/// kind that isn't one.
pub fn pack_of(kind: &str) -> Option<&str> {
    kind.strip_suffix(EFFECT)?
        .strip_suffix('/')
        .filter(|p| !p.is_empty() && !p.contains('/') && *p != "clap")
}

/// Why `id` can't be the pack a custom shader is saved as, or `None` when it can: a
/// pack ID (`a-z`, `0-9` and `-`, starting with a letter or digit, at most 32
/// characters) that no pack in `pack.list` has, loaded or not, and not `clap`, whose
/// kinds are the CLAP plugins' (ADR-0035).
pub fn save_as_problem(id: &str, packs: &[PackInfo]) -> Option<String> {
    let good = !id.is_empty()
        && id.len() <= 32
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !good {
        return Some(
            "A pack ID is 1 to 32 of a-z, 0-9 and -, starting with a letter or digit.".into(),
        );
    }
    if id == "clap" {
        return Some("`clap` is reserved for CLAP plugins.".into());
    }
    if packs.iter().any(|p| p.id == id || p.folder == id) {
        return Some(format!("A pack called `{id}` already exists."));
    }
    None
}

/// The first `custom-<n>` that no pack in `pack.list` has, loaded or not, so a new
/// shader never replaces one.
pub fn fresh_pack(packs: &[PackInfo]) -> String {
    (1u32..)
        .map(|n| format!("{PACK_PREFIX}{n}"))
        .find(|id| !packs.iter().any(|p| p.id == *id || p.folder == *id))
        .unwrap_or_else(|| format!("{PACK_PREFIX}0"))
}

/// What `pack.read` found in a custom shader's pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The shader's text.
    pub wgsl: String,
    /// Whether its manifest is the one the shader's annotations make. When it isn't (a
    /// pack written by hand), saving replaces it, which the editor says first.
    pub made_here: bool,
}

/// Opens pack `pack` in the editor from what `pack.read` returned: only a pack of
/// `effect.toml` and `shader.wgsl`, with nothing else. `Err` says why another pack
/// can't open here.
pub fn open(pack: &str, files: &[PackFileInfo]) -> Result<Opened, String> {
    let text = |path: &str| {
        files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.text.clone())
    };
    if files.len() != 2 {
        return Err(format!(
            "This pack has more files than `{MANIFEST_FILE}` and `{SHADER_FILE}`, so the shader \
             editor can’t open it. Edit its folder instead."
        ));
    }
    let (Some(manifest), Some(wgsl)) = (text(MANIFEST_FILE), text(SHADER_FILE)) else {
        return Err(format!(
            "The shader editor opens a pack of `{MANIFEST_FILE}` and `{SHADER_FILE}`. Edit \
             this one’s folder instead."
        ));
    };
    let made_here = declared(&wgsl).is_ok_and(|d| effect_toml(pack, &d) == manifest);
    Ok(Opened { wgsl, made_here })
}

/// The line of `shader.wgsl` a `pack.write` error names, if it names one: the engine
/// reports a shader's problem as `shader.wgsl: line <n>: …`.
pub fn error_line(message: &str) -> Option<usize> {
    let at = message.find(SHADER_FILE)?;
    let rest = message[at + SHADER_FILE.len()..]
        .trim_start_matches(':')
        .trim_start();
    let rest = rest.strip_prefix("line ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|&n| n > 0)
}

/// `text` with its UTF-16 range `start..end` (a text box's selection) replaced by
/// `insert`, and where the caret goes after it, in UTF-16 units.
pub fn splice_utf16(text: &str, start: u32, end: u32, insert: &str) -> (String, u32) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let len = units.len();
    let end = (end as usize).min(len);
    let start = (start as usize).min(end);
    let mut out: Vec<u16> = units[..start].to_vec();
    out.extend(insert.encode_utf16());
    let caret = out.len();
    out.extend_from_slice(&units[end..]);
    (
        String::from_utf16_lossy(&out),
        u32::try_from(caret).unwrap_or(u32::MAX),
    )
}

/// Where the editor's last compile stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Nothing changed since the shader opened.
    Opened,
    /// A change waits for its compile, or the compile is on its way.
    Compiling,
    /// The engine loaded the last change, and draws it.
    Compiled,
    /// The last change has a problem, at a line when it names one. The engine draws the
    /// last good version.
    Problem {
        /// The shader's line, from 1.
        line: Option<usize>,
        /// What is wrong.
        message: String,
    },
}

/// What closing the editor does first, by where its last change stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closing {
    /// Nothing is unsaved: it closes.
    Close,
    /// A change hasn't been sent yet, or is on its way: it is sent now, and the editor
    /// closes once it loads, or stays open showing its problem.
    Flush,
    /// The text has a problem, so the pack keeps the last version that loaded: the
    /// editor says so and closes only on Close anyway.
    Ask,
}

/// What closing the editor does at `status` (#1289 review B2: closing never drops a
/// change unsaid).
pub fn closing(status: &Status) -> Closing {
    match status {
        Status::Opened | Status::Compiled => Closing::Close,
        Status::Compiling => Closing::Flush,
        Status::Problem { .. } => Closing::Ask,
    }
}

/// The status line's text.
pub fn status_text(status: &Status) -> String {
    match status {
        Status::Opened => "Changes compile as you type.".into(),
        Status::Compiling => "Compiling…".into(),
        Status::Compiled => "Compiled. The picture shows this version.".into(),
        Status::Problem { message, .. } => {
            format!("{message} The picture keeps the last version that compiled.")
        }
    }
}

/// What a run of the shader's text is, for its colour in the editor (#1295).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    /// Anything without a colour of its own: names, operators, spaces.
    Plain,
    /// A `//` or `/* */` comment.
    Comment,
    /// A `// @name`, `// @param` or `// @renamed` line, which the editor reads.
    Annotation,
    /// A WGSL keyword (`fn`, `let`, `return`, …).
    Keyword,
    /// A built-in type (`f32`, `vec4`, `texture_2d`, …).
    Type,
    /// A number.
    Number,
    /// An attribute (`@fragment`, `@location`, …).
    Attribute,
    /// A name followed by `(`: a function call or declaration.
    Call,
}

impl Tok {
    /// The class of the span the editor draws this run in; `None` for plain text.
    pub fn class(self) -> Option<&'static str> {
        match self {
            Tok::Plain => None,
            Tok::Comment => Some("hl-comment"),
            Tok::Annotation => Some("hl-annotation"),
            Tok::Keyword => Some("hl-keyword"),
            Tok::Type => Some("hl-type"),
            Tok::Number => Some("hl-number"),
            Tok::Attribute => Some("hl-attribute"),
            Tok::Call => Some("hl-call"),
        }
    }
}

const KEYWORDS: &[&str] = &[
    "alias",
    "break",
    "case",
    "const",
    "const_assert",
    "continue",
    "continuing",
    "default",
    "diagnostic",
    "discard",
    "else",
    "enable",
    "false",
    "fn",
    "for",
    "if",
    "let",
    "loop",
    "override",
    "requires",
    "return",
    "struct",
    "switch",
    "true",
    "var",
    "while",
    "function",
    "private",
    "storage",
    "uniform",
    "workgroup",
    "read",
    "write",
    "read_write",
];

const TYPES: &[&str] = &[
    "bool",
    "f16",
    "f32",
    "i32",
    "u32",
    "array",
    "atomic",
    "ptr",
    "sampler",
    "sampler_comparison",
    "texture_1d",
    "texture_2d",
    "texture_2d_array",
    "texture_3d",
    "texture_cube",
    "texture_cube_array",
    "texture_multisampled_2d",
    "texture_depth_2d",
    "texture_storage_2d",
    "texture_external",
];

/// A built-in type name: one of [`TYPES`], or a vector or matrix (`vec4`, `vec3f`,
/// `mat4x4`, `mat3x3f`).
fn is_type(word: &str) -> bool {
    if TYPES.contains(&word) {
        return true;
    }
    let shape = |rest: &str| {
        let rest = rest.strip_suffix(['f', 'h', 'i', 'u']).unwrap_or(rest);
        matches!(rest, "2" | "3" | "4")
    };
    if let Some(rest) = word.strip_prefix("vec") {
        return shape(rest);
    }
    if let Some(rest) = word.strip_prefix("mat") {
        let rest = rest.strip_suffix(['f', 'h']).unwrap_or(rest);
        let b = rest.as_bytes();
        return b.len() == 3
            && matches!(b[0], b'2'..=b'4')
            && b[1] == b'x'
            && matches!(b[2], b'2'..=b'4');
    }
    false
}

/// `text` split into runs and what each is, in order: their concatenation is `text`.
/// A small WGSL tokenizer for the editor's colours, not a parser; the engine reports
/// what doesn't compile.
pub fn highlight(text: &str) -> Vec<(Tok, &str)> {
    let bytes = text.as_bytes();
    // Runs as `(tok, start, end)`; plain runs next to each other join.
    let mut runs: Vec<(Tok, usize, usize)> = Vec::new();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut at = 0;
    while at < bytes.len() {
        let b = bytes[at];
        let start = at;
        let tok = if bytes[at..].starts_with(b"//") {
            at = text[at..].find('\n').map_or(bytes.len(), |n| at + n);
            let line = text[start..at].trim_start_matches('/').trim_start();
            if ["@name", "@param", "@renamed"]
                .iter()
                .any(|a| line.starts_with(a))
            {
                Tok::Annotation
            } else {
                Tok::Comment
            }
        } else if bytes[at..].starts_with(b"/*") {
            // WGSL block comments nest.
            let mut depth = 0usize;
            while at < bytes.len() {
                if bytes[at..].starts_with(b"/*") {
                    depth += 1;
                    at += 2;
                } else if bytes[at..].starts_with(b"*/") {
                    depth -= 1;
                    at += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    at += 1;
                }
            }
            Tok::Comment
        } else if b == b'@' {
            at += 1;
            while at < bytes.len() && word(bytes[at]) {
                at += 1;
            }
            Tok::Attribute
        } else if b.is_ascii_digit()
            || (b == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            let hex = bytes[at..].starts_with(b"0x") || bytes[at..].starts_with(b"0X");
            at += if hex { 2 } else { 1 };
            while at < bytes.len() {
                let c = bytes[at];
                let exponent =
                    !hex && matches!(c, b'+' | b'-') && matches!(bytes[at - 1], b'e' | b'E');
                if word(c) || c == b'.' || exponent {
                    at += 1;
                } else {
                    break;
                }
            }
            Tok::Number
        } else if word(b) {
            while at < bytes.len() && word(bytes[at]) {
                at += 1;
            }
            let name = &text[start..at];
            if KEYWORDS.contains(&name) {
                Tok::Keyword
            } else if is_type(name) {
                Tok::Type
            } else if text[at..].trim_start_matches([' ', '\t']).starts_with('(') {
                Tok::Call
            } else {
                Tok::Plain
            }
        } else {
            // One character, whole even when it isn't ASCII.
            at += text[at..].chars().next().map_or(1, char::len_utf8);
            Tok::Plain
        };
        match runs.last_mut() {
            Some((Tok::Plain, _, end)) if tok == Tok::Plain => *end = at,
            _ => runs.push((tok, start, at)),
        }
    }
    runs.into_iter()
        .map(|(tok, start, end)| (tok, &text[start..end]))
        .collect()
}

/// The custom shader editor over the Filters window: the shader of pack `pack`, as
/// [`open`] read it. Undimmed, so the canvas shows each version as it compiles.
#[component]
pub fn ShaderEditor(
    /// The pack.
    pack: String,
    /// What `pack.read` found.
    opened: Opened,
    /// Closes the editor.
    close: impl Fn() + Copy + Send + Sync + 'static,
    /// Moves the filter being edited to the custom shader in the pack named, carrying
    /// its settings through the shader's renames, once Save as pack… has written and
    /// loaded it (#1295).
    save_as: impl Fn(String, Vec<(String, String)>) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let text = RwSignal::new(opened.wgsl);
    let status = RwSignal::new(Status::Opened);
    // Each change's number: a compile only reports for the latest change.
    let change = StoredValue::new(0u64);
    let pack = StoredValue::new(pack);
    let area = NodeRef::<leptos::html::Textarea>::new();
    let gutter = NodeRef::<leptos::html::Div>::new();
    // Closing waits for a change being sent; `unsaved` says the text hasn't compiled
    // and offers Close anyway.
    let flushing = RwSignal::new(false);
    let unsaved = RwSignal::new(false);
    let failed = move || {
        if flushing.get_untracked() {
            flushing.set(false);
            unsaved.set(true);
        }
    };
    let compile = move |mine: u64| {
        if change.try_get_value() != Some(mine) {
            return;
        }
        let Some(s) = session.get() else {
            return;
        };
        let pack_id = pack.get_value();
        let files = match pack_files(&pack_id, &text.get_untracked()) {
            Ok(files) => files,
            Err(error) => {
                status.set(Status::Problem {
                    line: Some(error.line),
                    message: format!("{error}."),
                });
                failed();
                return;
            }
        };
        spawn_local(async move {
            let action = Action::PackWrite {
                pack: pack_id.clone(),
                files,
            };
            let result = s.control.call(Call::Action { action }).await;
            let latest = change.try_get_value() == Some(mine);
            match result {
                Ok(_) => {
                    // The settings may have changed: read the kind again.
                    let kind = kind_of(&pack_id);
                    studio.descriptors.update(|d| _ = d.remove(&kind));
                    _ = studio.descriptor(&s, &kind).await;
                    studio.relist_kinds(&s).await;
                    if latest {
                        _ = status.try_set(Status::Compiled);
                        if flushing.try_get_untracked() == Some(true) {
                            close();
                        }
                    }
                }
                Err(error) if latest => {
                    let message = error.to_string();
                    _ = status.try_set(Status::Problem {
                        line: error_line(&message),
                        message,
                    });
                    if flushing.try_get_untracked().is_some() {
                        failed();
                    }
                }
                Err(_) => {}
            }
        });
    };
    let finish = move || match closing(&status.get_untracked()) {
        Closing::Close => close(),
        Closing::Flush => {
            flushing.set(true);
            unsaved.set(false);
            let mine = change.get_value() + 1;
            change.set_value(mine);
            compile(mine);
        }
        Closing::Ask => unsaved.set(true),
    };
    let changed = move |next: String| {
        unsaved.set(false);
        text.set(next);
        status.set(Status::Compiling);
        let mine = change.get_value() + 1;
        change.set_value(mine);
        set_timeout(move || compile(mine), COMPILE_AFTER);
    };
    let on_input = move |event: leptos::ev::Event| changed(event_target_value(&event));
    // Tab indents rather than leaving the box.
    let on_key = move |event: leptos::ev::KeyboardEvent| {
        if event.key() != "Tab" || event.shift_key() || event.ctrl_key() || event.alt_key() {
            return;
        }
        let Some(el) = area.get() else {
            return;
        };
        event.prevent_default();
        let start = el.selection_start().ok().flatten().unwrap_or(0);
        let end = el.selection_end().ok().flatten().unwrap_or(start);
        let (next, caret) = splice_utf16(&el.value(), start, end, INDENT);
        el.set_value(&next);
        _ = el.set_selection_range(caret, caret);
        changed(next);
    };
    let shade = NodeRef::<leptos::html::Pre>::new();
    // The copy has no scrollbars: sized to the text's box inside them (as CSSOM, which
    // the page's CSP allows), it scrolls as far as the text does, to the last pixel.
    let inner = RwSignal::new(None::<(i32, i32)>);
    let on_scroll = move |_| {
        if let Some(el) = area.get() {
            let size = Some((el.client_width(), el.client_height()));
            if inner.get_untracked() != size {
                inner.set(size);
            }
            if let Some(g) = gutter.get() {
                g.set_scroll_top(el.scroll_top());
            }
            if let Some(p) = shade.get() {
                p.set_scroll_top(el.scroll_top());
                p.set_scroll_left(el.scroll_left());
            }
        }
    };
    // The text in colour, under a textarea whose own text is transparent (#1295).
    let coloured = move || {
        text.with(|t| {
            let mut runs = highlight(t)
                .into_iter()
                .map(|(tok, run)| match tok.class() {
                    Some(class) => view! { <span class=class>{run.to_owned()}</span> }.into_any(),
                    None => run.to_owned().into_any(),
                })
                .collect::<Vec<_>>();
            // A last empty line still takes its height, as in the textarea.
            runs.push(" ".into_any());
            runs
        })
    };
    let bad_line = Memo::new(move |_| match status.get() {
        Status::Problem { line, .. } => line,
        _ => None,
    });
    let numbers = move || {
        let lines = text.with(|t| t.split('\n').count().max(1));
        let bad = bad_line.get();
        (1..=lines)
            .map(|n| {
                view! { <div class="shader-line" class:bad=bad == Some(n)>{n}</div> }
            })
            .collect_view()
    };
    let problem = move || matches!(status.get(), Status::Problem { .. });
    // A GPU hang quarantines the kind (ADR-0087), and a new version doesn't lift it.
    let held = Memo::new(move |_| {
        let kind = kind_of(&pack.get_value());
        studio.kinds.with(|k| {
            k.as_deref()
                .is_some_and(|k| k.iter().any(|k| k.kind_id == kind && k.quarantined))
        })
    });
    let reenable = move |_| {
        if let Some(s) = session.get() {
            studio.enable_effect(&s, kind_of(&pack.get_value()));
        }
    };
    // Save as pack…: the shader written again under a pack ID of the person's choosing,
    // and this filter moved to it. The old pack stays for any other filter of it.
    let naming = RwSignal::new(false);
    let new_id = RwSignal::new(String::new());
    let save_problem = RwSignal::new(None::<String>);
    let saving = RwSignal::new(false);
    let save = move || {
        let Some(s) = session.get() else {
            return;
        };
        let id = new_id.get_untracked().trim().to_owned();
        let wgsl = text.get_untracked();
        let found = pack_files(&id, &wgsl).and_then(|files| Ok((files, declared(&wgsl)?.renames)));
        let (files, renames) = match found {
            Ok(found) => found,
            Err(error) => {
                save_problem.set(Some(format!("The shader’s {error}.")));
                return;
            }
        };
        saving.set(true);
        save_problem.set(None);
        spawn_local(async move {
            let result = async {
                let packs = match query(&s, Query::PackList).await {
                    Ok(QueryResult::PackList { packs }) => packs,
                    Ok(_) => Vec::new(),
                    Err(error) => return Err(error.to_string()),
                };
                if let Some(problem) = save_as_problem(&id, &packs) {
                    return Err(problem);
                }
                let action = Action::PackWrite {
                    pack: id.clone(),
                    files,
                };
                s.control
                    .call(Call::Action { action })
                    .await
                    .map_err(|error| error.to_string())?;
                studio.relist_kinds(&s).await;
                Ok(renames)
            }
            .await;
            _ = saving.try_set(false);
            match result {
                Ok(renames) => {
                    save_as(id, renames);
                    close();
                }
                Err(error) => _ = save_problem.try_set(Some(error)),
            }
        });
    };
    let sub = format!("Effect pack “{}”", pack.get_value());
    view! {
        <Modal label="Custom shader" class="shader-editor" cancel=finish undimmed=true>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">"Custom shader"</h2>
                    <span class="modal-sub">{sub}</span>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| finish()>
                    "×"
                </button>
            </div>
            <div class="shader-body">
                {(!opened.made_here).then(|| view! {
                    <p class="add-note">
                        "This pack’s effect.toml wasn’t made by this editor. Your first change \
                         replaces it with one made from the shader’s // @name and // @param lines."
                    </p>
                })}
                <div class="shader-code">
                    <div class="shader-gutter" node_ref=gutter aria-hidden="true">{numbers}</div>
                    <div class="shader-stack">
                    <pre
                        class="shader-text shader-shade"
                        node_ref=shade
                        aria-hidden="true"
                        style:width=move || inner.get().map(|(w, _)| format!("{w}px"))
                        style:height=move || inner.get().map(|(_, h)| format!("{h}px"))
                    >{coloured}</pre>
                    <textarea
                        node_ref=area
                        class="shader-text shader-input"
                        aria-label="Shader (WGSL)"
                        spellcheck="false"
                        autocomplete="off"
                        wrap="off"
                        prop:value=move || text.get()
                        on:input=on_input
                        on:keydown=on_key
                        on:scroll=on_scroll
                    ></textarea>
                    </div>
                </div>
                <p class="shader-status" class:problem=problem role="status">
                    {move || status.with(status_text)}
                </p>
                {move || held.get().then(|| view! {
                    <p class="shader-status problem">
                        "A GPU hang was blamed on this shader, so its filters are bypassed until it is \
                         re-enabled. "
                        <button type="button" class="secondary" on:click=reenable>"Re-enable"</button>
                    </p>
                })}
                <p class="add-note">
                    "The // @name line names this shader in Add filter, where any item can use it."
                </p>
            </div>
            {move || naming.get().then(|| view! {
                <form
                    class="shader-save-as"
                    on:submit=move |ev| {
                        ev.prevent_default();
                        save();
                    }
                >
                    <label class="shader-save-as-row">
                        <span>"Pack ID"</span>
                        <input
                            type="text"
                            aria-label="Pack ID"
                            spellcheck="false"
                            autocomplete="off"
                            placeholder="my-look"
                            prop:value=move || new_id.get()
                            on:input=move |ev| new_id.set(event_target_value(&ev))
                        />
                        <button type="submit" class="primary" disabled=move || saving.get()>
                            {move || if saving.get() { "Saving…" } else { "Save" }}
                        </button>
                        <button type="button" class="secondary" on:click=move |_| naming.set(false)>
                            "Cancel"
                        </button>
                    </label>
                    <p class="add-note">
                        "Writes this shader as a new pack and moves this filter to it. The \
                         current pack stays, for any other filter that uses it."
                    </p>
                    {move || save_problem.get().map(|p| view! {
                        <p class="shader-status problem" role="alert">{p}</p>
                    })}
                </form>
            })}
            <div class="modal-footer">
                {move || (!naming.get() && !unsaved.get()).then(|| view! {
                    <button
                        type="button"
                        class="secondary shader-save-as-open"
                        on:click=move |_| {
                            save_problem.set(None);
                            naming.set(true);
                        }
                    >
                        "Save as pack…"
                    </button>
                })}
                {move || unsaved.get().then(|| view! {
                    <span class="shader-unsaved">
                        "This text hasn’t compiled, so the filter keeps the last version that did."
                    </span>
                    <button type="button" class="secondary" on:click=move |_| close()>
                        "Close anyway"
                    </button>
                })}
                <button
                    type="button"
                    class="primary"
                    disabled=move || flushing.get()
                    on:click=move |_| finish()
                >
                    {move || if flushing.get() { "Saving…" } else { "Done" }}
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {

    /// The runs of `text` that have a colour, as `(tok, run)`.
    fn coloured(text: &str) -> Vec<(Tok, &str)> {
        highlight(text)
            .into_iter()
            .filter(|(tok, _)| *tok != Tok::Plain)
            .collect()
    }

    #[test]
    fn highlighting_keeps_every_character_of_the_text() {
        for text in [TEMPLATE, "", "a", "/* open", "é → ü\n", "1e-3f + 0x1Fu"] {
            let joined: String = highlight(text).into_iter().map(|(_, run)| run).collect();
            assert_eq!(joined, text);
        }
    }

    #[test]
    fn highlighting_colours_wgsl_by_what_it_is() {
        let text = "// @param amount float 0..1 = 0.5 \"Amount\"\n// a note\n\
                    @fragment fn main(uv: vec2<f32>, m: mat4x4f) -> vec4f {\n\
                    \x20 let x = mix(1.5e-2, 0x1Fu, .5); /* a /* nested */ one */ return vec4f(x);\n}";
        assert_eq!(
            coloured(text),
            vec![
                (
                    Tok::Annotation,
                    "// @param amount float 0..1 = 0.5 \"Amount\""
                ),
                (Tok::Comment, "// a note"),
                (Tok::Attribute, "@fragment"),
                (Tok::Keyword, "fn"),
                (Tok::Call, "main"),
                (Tok::Type, "vec2"),
                (Tok::Type, "f32"),
                (Tok::Type, "mat4x4f"),
                (Tok::Type, "vec4f"),
                (Tok::Keyword, "let"),
                (Tok::Call, "mix"),
                (Tok::Number, "1.5e-2"),
                (Tok::Number, "0x1Fu"),
                (Tok::Number, ".5"),
                (Tok::Comment, "/* a /* nested */ one */"),
                (Tok::Keyword, "return"),
                (Tok::Type, "vec4f"),
            ]
        );
    }

    #[test]
    fn names_that_only_look_like_types_are_plain() {
        assert!(coloured("vec5 vec mat2x5 matrix vec4x").is_empty());
        assert_eq!(coloured("vec3h"), vec![(Tok::Type, "vec3h")]);
    }

    use super::*;

    fn line(message: &str, wgsl: &str) {
        let error = declared(wgsl).expect_err(message);
        assert!(
            error.message.contains(message),
            "`{}` should say `{message}`",
            error.message
        );
    }

    /// The template declares its name and two settings, which make a manifest of one
    /// effect with a float and a colour.
    #[test]
    fn the_template_declares_a_tint() {
        let d = declared(TEMPLATE).expect("the template parses");
        assert_eq!(d.name, "Custom shader");
        assert_eq!(
            d.params,
            vec![
                Param {
                    id: "amount".into(),
                    label: "Amount".into(),
                    kind: ParamKind::Float {
                        default: 0.5,
                        range: Some((0.0, 1.0)),
                    },
                },
                Param {
                    id: "tint".into(),
                    label: "Tint".into(),
                    kind: ParamKind::Color {
                        default: "#ff9a3cff".into(),
                    },
                },
            ]
        );
        let toml = effect_toml("custom-1", &d);
        assert!(toml.contains("[pack]\nid = \"custom-1\"\nname = \"Custom shader\""));
        assert!(toml.contains("[[effect]]\nid = \"shader\""));
        assert!(toml.contains("shader = \"shader.wgsl\""));
        assert!(toml.contains(
            "id = \"amount\"\nlabel = \"Amount\"\ntype = \"float\"\ndefault = 0.5\nmin = 0.0\nmax = 1.0\n"
        ));
        assert!(toml.contains("type = \"color\"\ndefault = \"#ff9a3cff\"\nalpha = true\n"));
    }

    /// The usage lines in the template's header are examples, not settings: only a
    /// comment that starts with `@param` declares one.
    #[test]
    fn only_a_comment_that_starts_with_param_declares_one() {
        let d =
            declared("// see `// @param x float = 1`\n//@paramx\nlet a = 1; // @param b").unwrap();
        assert!(d.params.is_empty());
    }

    /// Every type parses, with and without a range or a label.
    #[test]
    fn each_type_parses() {
        let d = declared(
            "// @name Wobble\n\
             // @param edge_width int 0..64 = 8 \"Edge (px)\"\n\
             // @param speed float = -1.5\n\
             // @param on bool = false\n\
             // @param ink color = #FF0000\n\
             // @param mode enum soft|hard_edge = hard_edge \"Mode\"\n",
        )
        .unwrap();
        assert_eq!(d.name, "Wobble");
        let kinds: Vec<_> = d
            .params
            .iter()
            .map(|p| (p.id.as_str(), p.label.as_str(), &p.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (
                    "edge_width",
                    "Edge (px)",
                    &ParamKind::Int {
                        default: 8,
                        range: Some((0, 64))
                    }
                ),
                (
                    "speed",
                    "Speed",
                    &ParamKind::Float {
                        default: -1.5,
                        range: None
                    }
                ),
                ("on", "On", &ParamKind::Bool { default: false }),
                (
                    "ink",
                    "Ink",
                    &ParamKind::Color {
                        default: "#ff0000".into()
                    }
                ),
                (
                    "mode",
                    "Mode",
                    &ParamKind::Enum {
                        options: vec!["soft".into(), "hard_edge".into()],
                        default: "hard_edge".into(),
                    }
                ),
            ]
        );
        let toml = effect_toml("custom-2", &d);
        assert!(toml.contains("type = \"int\"\ndefault = 8\nmin = 0\nmax = 64\n"));
        assert!(toml.contains("type = \"float\"\ndefault = -1.5\n\n[[effect.param]]"));
        assert!(toml.contains("type = \"color\"\ndefault = \"#ff0000\"\nalpha = false\n"));
        assert!(
            toml.contains(
                "[[effect.param.options]]\nvalue = \"hard_edge\"\nlabel = \"Hard edge\"\n"
            )
        );
    }

    /// Each `@renamed` line raises the version by one and lands in `renames`, so saved
    /// filters keep their values (ADR-0025 › Versioning a kind).
    #[test]
    fn a_rename_bumps_the_version_and_carries_the_value() {
        let plain = declared("// @param amount float = 0.5").unwrap();
        assert_eq!(plain.version(), 1);
        assert!(effect_toml("custom-1", &plain).contains("version = 1\n"));
        assert!(!effect_toml("custom-1", &plain).contains("renames"));

        let d = declared(
            "// @renamed amount strength\n\
             // @param strength float = 0.5\n\
             // @param tint color = #ffffff\n\
             // @renamed color tint\n",
        )
        .unwrap();
        assert_eq!(
            d.renames,
            vec![
                ("amount".to_owned(), "strength".to_owned()),
                ("color".to_owned(), "tint".to_owned())
            ]
        );
        assert_eq!(d.version(), 3);
        let toml = effect_toml("custom-1", &d);
        assert!(toml.contains(
            "version = 3\nshader = \"shader.wgsl\"\nrenames = { amount = \"strength\", color = \"tint\" }\n"
        ));
    }

    /// A rename's problem names its line: one that still declares the old ID, names no
    /// setting, or repeats an old ID.
    #[test]
    fn a_bad_rename_names_its_line() {
        let cases = [
            (
                "// @param a float = 1\n// @renamed a b",
                2,
                "`a` is still a setting",
            ),
            (
                "// @param b float = 1\n// @renamed a c",
                2,
                "`c` is not a setting",
            ),
            (
                "// @param b float = 1\n// @renamed a b\n// @renamed a b",
                3,
                "`a` is already renamed",
            ),
            ("// @renamed a", 1, "a rename is"),
            ("// @renamed a a", 1, "two different"),
            ("// @renamed A b", 1, "two different"),
        ];
        for (text, line, says) in cases {
            let error = declared(text).unwrap_err();
            assert_eq!(error.line, line, "{text}");
            assert!(error.message.contains(says), "{text}: {}", error.message);
        }
        // `@renamedx` isn't an annotation.
        assert!(declared("// @renamedx a b").unwrap().renames.is_empty());
    }

    #[test]
    fn save_as_takes_a_new_pack_id() {
        let packs = [PackInfo {
            id: "glow".into(),
            folder: "my-glow".into(),
            kinds: vec![],
            loaded: true,
            written_by: None,
        }];
        assert_eq!(save_as_problem("warm-look", &packs), None);
        assert_eq!(save_as_problem("2x", &packs), None);
        for (id, says) in [
            ("glow", "already exists"),
            ("my-glow", "already exists"),
            ("clap", "reserved"),
            ("", "1 to 32"),
            ("Warm", "1 to 32"),
            ("-warm", "1 to 32"),
            ("warm_look", "1 to 32"),
            (&"a".repeat(33), "1 to 32"),
        ] {
            let problem = save_as_problem(id, &packs).unwrap_or_default();
            assert!(problem.contains(says), "{id}: {problem}");
        }
    }

    /// A problem names its line and what is wrong, before anything is sent.
    #[test]
    fn a_bad_annotation_names_its_line() {
        let error = declared("fn a() {}\n\n// @param Big float = 1").unwrap_err();
        assert_eq!(error.line, 3);
        assert!(
            error
                .to_string()
                .starts_with("line 3: `Big` is not a setting ID")
        );
        line("has a default", "// @param a float 0..1");
        line("is not a type", "// @param a vec3 = 1");
        line(
            "the default is inside the range",
            "// @param a float 0..1 = 2",
        );
        line("min is below its max", "// @param a int 4..2 = 3");
        line("inside an i32", "// @param a int 0..99999999999 = 3");
        line("`true` or `false`", "// @param a bool = yes");
        line("`#rrggbb` or `#rrggbbaa`", "// @param a color = red");
        line("not one of the options", "// @param a enum x|y = z");
        line("listed twice", "// @param a enum x|x = x");
        line(
            "another setting is called `a`",
            "// @param a bool = true\n// @param a bool = false",
        );
        line("one `@name` line", "// @name A\n// @name B");
        line("double quotes", "// @param a bool = true Label\"");
        line("too many words", "// @param a float 0..1 extra = 0");
    }

    /// A name and labels with quotes or backslashes stay valid TOML strings.
    #[test]
    fn names_are_escaped_in_the_manifest() {
        let d = declared("// @name Say \"hi\" \\ there\n// @param a bool = true \"A\\B\"").unwrap();
        let toml = effect_toml("custom-1", &d);
        assert!(toml.contains(r#"name = "Say \"hi\" \\ there""#));
        assert!(toml.contains(r#"label = "A\\B""#));
    }

    /// Floats are always written as TOML floats.
    #[test]
    fn floats_keep_their_point() {
        assert_eq!(toml_float(1.0), "1.0");
        assert_eq!(toml_float(-2.0), "-2.0");
        assert_eq!(toml_float(0.25), "0.25");
        assert_eq!(toml_float(1e-7), "1e-7");
    }

    /// A write sends the manifest and the shader; a bad annotation sends nothing.
    #[test]
    fn a_write_holds_the_manifest_and_the_shader() {
        let files = pack_files("custom-3", TEMPLATE).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["effect.toml", "shader.wgsl"]);
        assert_eq!(files[1].content, PackContent::Text(TEMPLATE.into()));
        assert!(pack_files("custom-3", "// @param a").is_err());
    }

    /// A custom shader's kind and pack go both ways, and other kinds aren't one.
    #[test]
    fn kinds_name_their_pack() {
        assert_eq!(kind_of("custom-4"), "custom-4/shader");
        assert_eq!(pack_of("custom-4/shader"), Some("custom-4"));
        assert_eq!(pack_of("crt/shader"), Some("crt"));
        assert_eq!(pack_of("crt/scanlines"), None);
        assert_eq!(pack_of("shader"), None);
        assert_eq!(pack_of("/shader"), None);
        assert_eq!(pack_of("clap/shader"), None);
    }

    fn info(id: &str, folder: &str) -> PackInfo {
        PackInfo {
            id: id.into(),
            folder: folder.into(),
            kinds: Vec::new(),
            loaded: false,
            written_by: None,
        }
    }

    /// A new shader takes the first number no pack or folder has, so it never replaces
    /// a pack, even one that doesn't load.
    #[test]
    fn a_new_shader_never_takes_a_packs_id() {
        assert_eq!(fresh_pack(&[]), "custom-1");
        assert_eq!(
            fresh_pack(&[
                info("custom-1", "custom-1"),
                info("x", "custom-2"),
                info("custom-3", "c")
            ]),
            "custom-4"
        );
    }

    fn file(path: &str, text: &str) -> PackFileInfo {
        PackFileInfo {
            path: path.into(),
            size: text.len() as u64,
            digest: String::new(),
            text: Some(text.into()),
            base64: None,
        }
    }

    /// A pack this editor wrote opens as made here; one with another manifest opens
    /// with a warning; one with other files doesn't open.
    #[test]
    fn a_pack_opens_when_it_is_a_manifest_and_a_shader() {
        let made = effect_toml("custom-1", &declared(TEMPLATE).unwrap());
        let opened = open(
            "custom-1",
            &[file("effect.toml", &made), file("shader.wgsl", TEMPLATE)],
        )
        .unwrap();
        assert_eq!(
            opened,
            Opened {
                wgsl: TEMPLATE.into(),
                made_here: true
            }
        );
        let by_hand = open(
            "custom-1",
            &[
                file("effect.toml", "[pack]\n"),
                file("shader.wgsl", TEMPLATE),
            ],
        )
        .unwrap();
        assert!(!by_hand.made_here);
        assert!(open("crt", &[file("effect.toml", ""), file("crt.wgsl", "")]).is_err());
        assert!(
            open(
                "crt",
                &[
                    file("effect.toml", ""),
                    file("shader.wgsl", ""),
                    file("a.png", "")
                ]
            )
            .is_err()
        );
    }

    /// Tab's spaces go in at the selection, counted in UTF-16 as the text box counts.
    #[test]
    fn tab_replaces_the_selection() {
        assert_eq!(splice_utf16("ab", 1, 1, "  "), ("a  b".into(), 3));
        assert_eq!(splice_utf16("abcd", 1, 3, "-"), ("a-d".into(), 2));
        // "é" and "😀" are one and two UTF-16 units.
        assert_eq!(splice_utf16("é😀x", 3, 3, "_"), ("é😀_x".into(), 4));
        assert_eq!(splice_utf16("ab", 9, 9, "!"), ("ab!".into(), 3));
    }

    /// Closing sends a change still waiting, asks before leaving text that doesn't
    /// compile, and otherwise just closes (#1289 review B2).
    #[test]
    fn closing_never_drops_a_change_unsaid() {
        assert_eq!(closing(&Status::Opened), Closing::Close);
        assert_eq!(closing(&Status::Compiled), Closing::Close);
        assert_eq!(closing(&Status::Compiling), Closing::Flush);
        assert_eq!(
            closing(&Status::Problem {
                line: None,
                message: String::new()
            }),
            Closing::Ask
        );
    }

    /// A problem keeps the picture's last good version, and says so.
    #[test]
    fn a_problem_says_the_picture_keeps_the_last_version() {
        let text = status_text(&Status::Problem {
            line: Some(3),
            message: "line 3: bad.".into(),
        });
        assert_eq!(
            text,
            "line 3: bad. The picture keeps the last version that compiled."
        );
    }

    /// The line of a shader error, as the engine words it.
    #[test]
    fn a_shader_error_names_its_line() {
        assert_eq!(
            error_line("invalid argument at `files`: shader.wgsl: line 14: expected `;`"),
            Some(14)
        );
        assert_eq!(error_line("effect.toml: pack.id: bad"), None);
        assert_eq!(error_line("shader.wgsl: no entry point `fs`"), None);
    }
}
