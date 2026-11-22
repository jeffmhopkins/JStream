//! The editor for a `looks` setting (`ParamType::Looks`, ADR-0171 decision 9): a `grid`
//! source's table from a cell's state text to how the cell looks. One row a state, with
//! its colour (or the cell's own), brightness and blink rate, a remove button, a row to
//! add a state, and a preset that fills the clip launcher states JTunnel's state
//! documents. The table is edited as a whole value: each change sends the new object.

use jstream_control::protocol::{LOOK_ABSENT, LOOK_CELL_COLOR, MAX_LOOK_NAME, MAX_LOOKS};
use leptos::prelude::*;
use serde_json::{Map, Value, json};

/// One look, as the editor shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct LookRow {
    /// The state text.
    pub state: String,
    /// `None` for the cell's own colour.
    pub color: Option<String>,
    /// 0 to 1.
    pub brightness: f64,
    /// Blinks a second; 0 is steady.
    pub blink_hz: f64,
}

/// The rows of a looks value, in its order.
#[must_use]
pub fn rows(value: &Value) -> Vec<LookRow> {
    value
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(state, look)| LookRow {
                    state: state.clone(),
                    color: look
                        .get("color")
                        .and_then(Value::as_str)
                        .filter(|c| *c != LOOK_CELL_COLOR)
                        .map(str::to_owned),
                    brightness: look
                        .get("brightness")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0),
                    blink_hz: look.get("blink_hz").and_then(Value::as_f64).unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The looks value of `rows`; a later row with an earlier one's state replaces it.
#[must_use]
pub fn value(rows: &[LookRow]) -> Value {
    let mut map = Map::new();
    for row in rows {
        let mut look = Map::new();
        look.insert(
            "color".into(),
            Value::from(row.color.clone().unwrap_or_else(|| LOOK_CELL_COLOR.into())),
        );
        look.insert("brightness".into(), json!(row.brightness.clamp(0.0, 1.0)));
        if row.blink_hz > 0.0 {
            look.insert("blink_hz".into(), json!(row.blink_hz.min(16.0)));
        }
        map.insert(row.state.clone(), Value::Object(look));
    }
    Value::Object(map)
}

/// The clip launcher looks for the states JTunnel's state sends for a clip slot
/// (`state`, and `queued` to blink): with `state_field` set to `queued,state`, a grid
/// draws a launcher as a DAW shows it.
#[must_use]
pub fn clip_launcher() -> Value {
    json!({
        LOOK_ABSENT: {"color": "#141414", "brightness": 1.0},
        "empty": {"color": "#202020", "brightness": 1.0},
        "stopped": {"color": "cell", "brightness": 0.35},
        "playing": {"color": "cell", "brightness": 1.0},
        "recording": {"color": "#ff2a2a", "brightness": 1.0},
        "play": {"color": "cell", "brightness": 1.0, "blink_hz": 4.0},
        "record": {"color": "#ff2a2a", "brightness": 1.0, "blink_hz": 4.0},
        "stop": {"color": "cell", "brightness": 0.6, "blink_hz": 4.0},
    })
}

/// Why `rows` can't be sent, if it can't.
fn problem(rows: &[LookRow]) -> Option<String> {
    if rows.len() > MAX_LOOKS {
        return Some(format!("At most {MAX_LOOKS} states."));
    }
    if rows
        .iter()
        .any(|r| r.state.is_empty() || r.state.len() > MAX_LOOK_NAME)
    {
        return Some(format!("A state is 1 to {MAX_LOOK_NAME} bytes."));
    }
    rows.iter()
        .enumerate()
        .any(|(i, r)| rows[..i].iter().any(|o| o.state == r.state))
        .then(|| "Each state once.".to_owned())
}

/// A change to the table's rows.
type Change = Box<dyn FnOnce(&mut Vec<LookRow>)>;
type Edit = dyn Fn(Change) + Send + Sync;

/// The editor.
pub fn looks_input(
    value_now: impl Fn() -> Value + Clone + Send + Sync + 'static,
    send: impl Fn(Result<Value, String>) + Clone + Send + Sync + 'static,
) -> AnyView {
    let edit = {
        let (value_now, send) = (value_now.clone(), send.clone());
        move |change: Change| {
            let mut list = rows(&value_now());
            change(&mut list);
            match problem(&list) {
                Some(p) => send(Err(p)),
                None => send(Ok(value(&list))),
            }
        }
    };
    let table = {
        let (value_now, edit) = (value_now.clone(), edit.clone());
        move || {
            rows(&value_now())
                .into_iter()
                .enumerate()
                .map(|(i, row)| look_row(i, row, edit.clone()))
                .collect_view()
        }
    };
    let add = {
        let edit = edit.clone();
        move |_| {
            edit(Box::new(|list: &mut Vec<LookRow>| {
                let mut n = list.len() + 1;
                while list.iter().any(|r| r.state == format!("state{n}")) {
                    n += 1;
                }
                list.push(LookRow {
                    state: format!("state{n}"),
                    color: Some("#ffffff".into()),
                    brightness: 1.0,
                    blink_hz: 0.0,
                });
            }));
        }
    };
    let preset = move |_| send(Ok(clip_launcher()));
    view! {
        <div class="looks">
            <div class="looks-head">
                <span>"State"</span><span>"Colour"</span><span>"Brightness"</span>
                <span>"Blink (Hz)"</span><span></span>
            </div>
            {table}
            <div class="looks-actions">
                <button type="button" class="sm" on:click=add>"Add a state"</button>
                <button type="button" class="sm" title="Replaces the table with the states a JTunnel clip slot sends"
                    on:click=preset>"Clip launcher looks"</button>
            </div>
        </div>
    }
    .into_any()
}

fn look_row(
    i: usize,
    row: LookRow,
    edit: impl Fn(Change) + Clone + Send + Sync + 'static,
) -> AnyView {
    let edit: std::sync::Arc<Edit> = std::sync::Arc::new(edit);
    let on = |f: fn(&mut LookRow, String)| {
        let edit = edit.clone();
        move |event: leptos::ev::Event| {
            let text = event_target_value(&event);
            edit(Box::new(move |list: &mut Vec<LookRow>| {
                if let Some(r) = list.get_mut(i) {
                    f(r, text);
                }
            }));
        }
    };
    let own = row.color.is_none();
    let on_own = {
        let edit = edit.clone();
        move |event: leptos::ev::Event| {
            let checked = event_target_checked(&event);
            edit(Box::new(move |list: &mut Vec<LookRow>| {
                if let Some(r) = list.get_mut(i) {
                    r.color = if checked {
                        None
                    } else {
                        Some("#ffffff".into())
                    };
                }
            }));
        }
    };
    let remove = {
        let edit = edit.clone();
        move |_| {
            edit(Box::new(move |list: &mut Vec<LookRow>| {
                if i < list.len() {
                    list.remove(i);
                }
            }));
        }
    };
    let color = row.color.clone().unwrap_or_else(|| "#ffffff".into());
    view! {
        <div class="looks-row">
            <input type="text" maxlength=MAX_LOOK_NAME.to_string() prop:value=row.state.clone()
                on:change=on(|r, t| r.state = t.trim().to_owned()) />
            <span class="looks-color">
                <label title="The cell's own colour from the data">
                    <input type="checkbox" prop:checked=own on:change=on_own />"Own"
                </label>
                {(!own).then(|| view! {
                    <input type="color" prop:value=color.clone()
                        on:change=on(|r, t| r.color = Some(t)) />
                })}
            </span>
            <input type="number" min="0" max="1" step="0.05" prop:value=row.brightness.to_string()
                on:change=on(|r, t| r.brightness = t.parse().unwrap_or(1.0)) />
            <input type="number" min="0" max="16" step="0.5" prop:value=row.blink_hz.to_string()
                on:change=on(|r, t| r.blink_hz = t.parse().unwrap_or(0.0)) />
            <button type="button" class="icon" title="Remove this state" on:click=remove>"×"</button>
        </div>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_value_round_trip_and_cell_is_no_colour() {
        let v = clip_launcher();
        let r = rows(&v);
        assert!(r.iter().any(|r| r.state == "playing" && r.color.is_none()));
        assert!(r.iter().any(|r| r.state == "play" && r.blink_hz == 4.0));
        assert_eq!(rows(&value(&r)), r);
    }

    #[test]
    fn an_empty_state_or_too_many_is_a_problem() {
        let row = |s: &str| LookRow {
            state: s.into(),
            color: None,
            brightness: 1.0,
            blink_hz: 0.0,
        };
        assert!(problem(&[row("")]).is_some());
        assert!(problem(&[row("ok")]).is_none());
        let many: Vec<_> = (0..=MAX_LOOKS).map(|i| row(&format!("s{i}"))).collect();
        assert!(problem(&many).is_some());
        assert!(problem(&[row("a"), row("a")]).is_some(), "each state once");
    }
}
