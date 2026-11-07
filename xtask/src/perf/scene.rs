//! The perf scenes: JSON files under `xtask/perf/`, each one scene or an array of them,
//! expanded into the edits of a `batch` call.
//!
//! A scene is `{"name", "about", "edits", "needs"?, "cycles"?}`. The runner creates the
//! scene `name` itself, before the edits. An entry of `edits` is an edit, or a repeat
//! block `{"each": {"count", "cols"?, "cycle"?}, "edits": [...]}` that adds its edits
//! once per index `i` in `0..count`. Inside the edits:
//!
//! - `{i}` in a string becomes the index, and the string `"$i"` the index as a number;
//! - `"$asset:<file>"` becomes a reference to a file the run wrote ([`super::assets`]);
//! - `"$cell"` becomes the geometry of cell `i` in a grid of `count` cells, `cols` wide,
//!   over the canvas, and `"$canvas"` the whole canvas. As an object key, `{"$cell": 1}`
//!   or `{"$canvas": 0.8}` (a scale about the centre), the other keys are added to it;
//! - `"$<name>"` becomes entry `i` (wrapping) of the block's `cycle.<name>` list.
//!
//! Any other string starting with `$` is an error, so a typo fails at load, not as an
//! engine error halfway through a run.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use super::assets;

/// Most edits sent in one `batch` call.
pub const BATCH_LIMIT: usize = 200;

/// A loaded scene, its edits expanded.
#[derive(Debug, Clone)]
pub struct Scene {
    /// Its scene ID and its row's name.
    pub name: String,
    /// One line on what it loads the engine with.
    pub about: String,
    /// Whether it needs the browser helper.
    pub needs_browser: bool,
    /// Build, show, leave and delete it this many times, then measure the base scene;
    /// `None` measures the scene itself, once.
    pub cycles: Option<u32>,
    /// The edits that build it, its own `scene.create` first.
    pub edits: Vec<Value>,
}

impl Scene {
    /// What to delete to remove what [`Scene::edits`] made, as `delete.plan` targets:
    /// the scene itself first (nothing refers to it), then the other scenes and the
    /// sources, newest first. Each target's plan also removes what refers to it, so a
    /// later target may already be gone.
    pub fn teardown(&self) -> Vec<Value> {
        let mut out = vec![json!({"type": "scene", "scene_id": self.name})];
        for e in self.edits.iter().skip(1).rev() {
            match e["type"].as_str() {
                Some("scene.create") => {
                    out.push(json!({"type": "scene", "scene_id": e["scene_id"]}));
                }
                Some("source.create") => {
                    out.push(json!({"type": "source", "source_id": e["source_id"]}));
                }
                _ => {}
            }
        }
        out
    }
}

/// Loads every scene in the `*.json` files of `dir`, in file-name order, for a canvas of
/// `canvas` pixels.
pub fn load_dir(dir: &Path, canvas: (f64, f64)) -> Result<Vec<Scene>> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut scenes = Vec::new();
    for file in files {
        let text =
            fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let value: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
        let list = match value {
            Value::Array(list) => list,
            one => vec![one],
        };
        for raw in &list {
            scenes.push(parse(raw, canvas).with_context(|| format!("in {}", file.display()))?);
        }
    }
    let mut names: Vec<&str> = scenes.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
        bail!("two scenes are named {}", w[0]);
    }
    Ok(scenes)
}

/// Parses and expands one scene.
pub fn parse(raw: &Value, canvas: (f64, f64)) -> Result<Scene> {
    let text = |key: &str| -> Result<String> {
        raw.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("a scene needs a string `{key}`"))
    };
    let name = text("name")?;
    let about = text("about")?;
    let needs_browser = match raw.get("needs").and_then(Value::as_str) {
        None => false,
        Some("browser") => true,
        Some(other) => bail!("scene {name}: unknown need `{other}`"),
    };
    let cycles = match raw.get("cycles") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .with_context(|| format!("scene {name}: `cycles` is a positive count"))?,
        ),
    };
    let entries = raw
        .get("edits")
        .and_then(Value::as_array)
        .with_context(|| format!("scene {name}: `edits` is an array"))?;
    let mut edits = vec![json!({"type": "scene.create", "scene_id": name, "name": name})];
    for entry in entries {
        expand(entry, canvas, &mut edits).with_context(|| format!("scene {name}"))?;
    }
    for key in raw.as_object().map(Map::keys).into_iter().flatten() {
        if !["name", "about", "needs", "cycles", "edits"].contains(&key.as_str()) {
            bail!("scene {name}: unknown key `{key}`");
        }
    }
    Ok(Scene {
        name,
        about,
        needs_browser,
        cycles,
        edits,
    })
}

/// A repeat block's index and grid, for the substitutions that need one.
struct Cell<'a> {
    i: usize,
    count: usize,
    cols: usize,
    cycle: Option<&'a Map<String, Value>>,
}

fn expand(entry: &Value, canvas: (f64, f64), out: &mut Vec<Value>) -> Result<()> {
    let Some(each) = entry.get("each") else {
        out.push(substitute(entry, canvas, None)?);
        return Ok(());
    };
    let count = each
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .context("`each.count` is a count")?;
    let cols = match each.get("cols") {
        Some(c) => c
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .context("`each.cols` is a positive count")?,
        None => (1..=count.max(1)).find(|c| c * c >= count).unwrap_or(1),
    };
    let cycle = match each.get("cycle") {
        None => None,
        Some(c) => {
            let map = c
                .as_object()
                .context("`each.cycle` is an object of lists")?;
            if map.values().any(|v| v.as_array().is_none_or(Vec::is_empty)) {
                bail!("each `each.cycle` entry is a non-empty list");
            }
            Some(map)
        }
    };
    let edits = entry
        .get("edits")
        .and_then(Value::as_array)
        .context("a repeat block needs an `edits` array")?;
    for i in 0..count {
        let cell = Cell {
            i,
            count,
            cols,
            cycle,
        };
        for edit in edits {
            out.push(substitute(edit, canvas, Some(&cell))?);
        }
    }
    Ok(())
}

/// Geometry centred in a `w`×`h` box at (`x`, `y`).
fn geometry(x: f64, y: f64, w: f64, h: f64) -> Value {
    json!({"center": {"x": x, "y": y}, "size": {"width": w, "height": h}})
}

/// The geometry of a grid cell, 4 pixels smaller than the cell on each axis.
fn cell_geometry(cell: &Cell<'_>, canvas: (f64, f64)) -> Value {
    let rows = cell.count.div_ceil(cell.cols).max(1);
    let (cw, ch) = (canvas.0 / cell.cols as f64, canvas.1 / rows as f64);
    let (c, r) = ((cell.i % cell.cols) as f64, (cell.i / cell.cols) as f64);
    geometry(cw * c + cw / 2.0, ch * r + ch / 2.0, cw - 4.0, ch - 4.0)
}

fn canvas_geometry(canvas: (f64, f64), scale: f64) -> Value {
    geometry(
        canvas.0 / 2.0,
        canvas.1 / 2.0,
        canvas.0 * scale,
        canvas.1 * scale,
    )
}

fn substitute(value: &Value, canvas: (f64, f64), cell: Option<&Cell<'_>>) -> Result<Value> {
    let need_cell = |what: &str| cell.with_context(|| format!("`{what}` outside a repeat block"));
    Ok(match value {
        Value::String(s) if s == "$i" => json!(need_cell("$i")?.i),
        Value::String(s) if s == "$cell" => cell_geometry(need_cell("$cell")?, canvas),
        Value::String(s) if s == "$canvas" => canvas_geometry(canvas, 1.0),
        Value::String(s) if s.starts_with("$asset:") => json!({
            "type": "asset",
            "root": "collection",
            "path": format!("{}/{}", assets::DIR, &s["$asset:".len()..]),
        }),
        Value::String(s) if s.starts_with('$') => {
            let c = need_cell(s)?;
            let list = c
                .cycle
                .and_then(|m| m.get(&s[1..]))
                .and_then(Value::as_array)
                .with_context(|| format!("`{s}` names no `each.cycle` list"))?;
            // A cycle entry may hold `$asset:` and `$canvas`, but not another cycle.
            substitute(&list[c.i % list.len()], canvas, None)?
        }
        Value::String(s) => match cell {
            Some(c) => Value::String(s.replace("{i}", &c.i.to_string())),
            None if s.contains("{i}") => bail!("`{{i}}` outside a repeat block in {s:?}"),
            None => value.clone(),
        },
        Value::Array(list) => Value::Array(
            list.iter()
                .map(|v| substitute(v, canvas, cell))
                .collect::<Result<_>>()?,
        ),
        Value::Object(map) => {
            let base = if map.contains_key("$cell") {
                Some(cell_geometry(need_cell("$cell")?, canvas))
            } else if let Some(scale) = map.get("$canvas") {
                let scale = scale.as_f64().context("`$canvas` takes a scale")?;
                Some(canvas_geometry(canvas, scale))
            } else {
                None
            };
            let mut out = match base {
                Some(Value::Object(b)) => b,
                _ => Map::new(),
            };
            for (k, v) in map {
                if k != "$cell" && k != "$canvas" {
                    out.insert(k.clone(), substitute(v, canvas, cell)?);
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    })
}

/// Splits edits into `batch` calls of at most [`BATCH_LIMIT`].
pub fn batches(edits: &[Value]) -> impl Iterator<Item = &[Value]> {
    edits.chunks(BATCH_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: (f64, f64) = (1000.0, 500.0);

    #[test]
    fn repeat_blocks_expand_with_cells_cycles_and_assets() {
        let raw = json!({
            "name": "s",
            "about": "x",
            "edits": [
                {"type": "source.create", "source_id": "img", "settings": {"file": "$asset:still.png"}},
                {"each": {"count": 4, "cols": 2, "cycle": {"turn": [0, 15]}},
                 "edits": [{"type": "item.add", "item_id": "i_{i}", "n": "$i",
                            "geometry": {"$cell": 1, "turn_deg": "$turn"}}]}
            ]
        });
        let scene = parse(&raw, CANVAS).expect("parses");
        assert_eq!(scene.edits.len(), 6);
        assert_eq!(scene.edits[0]["type"], "scene.create");
        assert_eq!(
            scene.edits[1]["settings"]["file"],
            json!({"type": "asset", "root": "collection", "path": "perf-assets/still.png"})
        );
        let last = &scene.edits[5];
        assert_eq!(last["item_id"], "i_3");
        assert_eq!(last["n"], 3);
        assert_eq!(last["geometry"]["turn_deg"], 15);
        assert_eq!(last["geometry"]["center"], json!({"x": 750.0, "y": 375.0}));
        assert_eq!(
            last["geometry"]["size"],
            json!({"width": 496.0, "height": 246.0})
        );
    }

    #[test]
    fn canvas_scales_about_the_centre() {
        let raw = json!({"name": "s", "about": "x",
            "edits": [{"g": {"$canvas": 0.5, "rotation_deg": 3}, "h": "$canvas"}]});
        let scene = parse(&raw, CANVAS).expect("parses");
        let e = &scene.edits[1];
        assert_eq!(e["g"]["size"], json!({"width": 500.0, "height": 250.0}));
        assert_eq!(e["g"]["rotation_deg"], 3);
        assert_eq!(e["h"]["center"], json!({"x": 500.0, "y": 250.0}));
    }

    #[test]
    fn typos_and_misplaced_markers_fail_at_load() {
        for edits in [
            json!([{"g": "$cell"}]),
            json!([{"t": "a{i}"}]),
            json!([{"each": {"count": 2}, "edits": [{"c": "$colour"}]}]),
            json!([{"each": {"count": 2, "cycle": {"c": []}}, "edits": []}]),
        ] {
            let raw = json!({"name": "s", "about": "x", "edits": edits});
            assert!(parse(&raw, CANVAS).is_err(), "{edits}");
        }
        let raw = json!({"name": "s", "about": "x", "edits": [], "cycle": 3});
        assert!(parse(&raw, CANVAS).is_err());
    }

    #[test]
    fn teardown_names_the_scene_then_the_rest_newest_first() {
        let raw = json!({"name": "s", "about": "x", "edits": [
            {"type": "source.create", "source_id": "a"},
            {"type": "scene.create", "scene_id": "inner"},
            {"type": "item.add", "item_id": "i"},
            {"type": "source.create", "source_id": "b"}
        ]});
        let scene = parse(&raw, CANVAS).expect("parses");
        assert_eq!(
            scene.teardown(),
            vec![
                json!({"type": "scene", "scene_id": "s"}),
                json!({"type": "source", "source_id": "b"}),
                json!({"type": "scene", "scene_id": "inner"}),
                json!({"type": "source", "source_id": "a"}),
            ]
        );
    }

    #[test]
    fn the_shipped_scenes_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("perf");
        let scenes = load_dir(&dir, (1920.0, 1080.0)).expect("the scenes load");
        assert!(scenes.len() >= 30, "{} scenes", scenes.len());
    }
}
