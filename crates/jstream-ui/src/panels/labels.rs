//! Labels (spec-panels.md S4, S7, S8, S20 and I1): the chips a source's rows and the
//! Inspector's header show, the popover that puts labels on a source or takes them off,
//! and the label manager, an inline card in the Scene objects panel that names, colours,
//! gives icons to, adds and deletes labels. Labels are collection state (ADR-0024 §4):
//! sources carry them, so a reference shares its original's, and scenes carry none.
//! The edits each control sends are plain functions, tested natively.
//!
//! Docs: docs/UI-FEATURES.md#scenes, docs/UI-FEATURES.md#objects

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::icons::{Icon, named};
use crate::store::{Collection, Studio};
use crate::workspace::DockState;
use jstream_control::protocol::{Edit, LabelId, LabelInfo, SourceId, SourceMeta};
use leptos::prelude::*;

/// The label colours (G9).
pub const SWATCHES: [&str; 8] = [
    "#4C8DFF", "#2EC4D6", "#3FB27F", "#E8A33D", "#E5484D", "#E86BA6", "#B07CFF", "#9AA1AC",
];

/// The icon picker's groups and their icons (G8).
pub const ICON_GROUPS: [(&str, &[&str]); 3] = [
    (
        "Sources",
        &[
            "monitor", "window", "webcam", "spout", "mic", "speaker", "globe", "image", "text",
            "film", "palette", "music", "wave", "bars",
        ],
    ),
    (
        "Scenes",
        &[
            "layers", "folder", "play", "coffee", "chat", "gamepad", "flag", "clock", "user",
            "hash",
        ],
    ),
    (
        "General",
        &[
            "star", "heart", "bolt", "sparkle", "bell", "tag", "eye", "lock", "wrench",
        ],
    ),
];

/// The icon a label shows: its own, or the tag.
pub fn label_icon(label: &LabelInfo) -> &'static str {
    label
        .icon
        .as_deref()
        .map_or("tag", |icon| named(icon, "tag"))
}

/// A label's colour as a chip's tint: the colour at 22 % opacity.
pub fn tint(color: &str) -> String {
    format!("{color}38")
}

/// The metadata with `label` on or off, the labels kept ordered by ID as the engine
/// stores them.
pub fn with_label(meta: &SourceMeta, label: &LabelId, on: bool) -> SourceMeta {
    let mut labels: Vec<LabelId> = meta
        .labels
        .iter()
        .filter(|l| *l != label)
        .cloned()
        .collect();
    if on {
        labels.push(label.clone());
        labels.sort();
    }
    SourceMeta {
        labels,
        favourite: meta.favourite,
    }
}

/// The edit that puts `label` on the source or takes it off, or `None` when it already
/// is as asked or the source is gone.
pub fn toggle_edit(collection: &Collection, source_id: &SourceId, label: &LabelId) -> Option<Edit> {
    let source = collection.source(source_id)?;
    let on = !source.meta.labels.contains(label);
    Some(Edit::SourceSetMeta {
        source_id: source_id.clone(),
        meta: with_label(&source.meta, label, on),
    })
}

/// The edit that adds a label as the prototype's "+ New label" does: "New label", the
/// tag icon and the next swatch.
pub fn new_label_edit(collection: &Collection, label_id: LabelId) -> Edit {
    Edit::LabelCreate {
        label_id,
        name: "New label".into(),
        color: SWATCHES[collection.labels.len() % SWATCHES.len()].into(),
        icon: Some("tag".into()),
    }
}

/// The edits that delete a label: taken off every source that carries it, then deleted,
/// in one batch so it is one undo step.
pub fn delete_edits(collection: &Collection, label: &LabelId) -> Vec<Edit> {
    collection
        .sources
        .iter()
        .filter(|s| s.meta.labels.contains(label))
        .map(|s| Edit::SourceSetMeta {
            source_id: s.source_id.clone(),
            meta: with_label(&s.meta, label, false),
        })
        .chain(std::iter::once(Edit::LabelDelete {
            label_id: label.clone(),
        }))
        .collect()
}

/// The edit that changes a label's name, colour or icon, keeping the rest; `None` when
/// nothing changes or the name is blank.
pub fn update_edit(
    label: &LabelInfo,
    name: Option<&str>,
    color: Option<&str>,
    icon: Option<&str>,
) -> Option<Edit> {
    let name = name.map_or(label.name.as_str(), str::trim);
    let color = color.unwrap_or(&label.color);
    let icon = icon.or(label.icon.as_deref());
    let same = name == label.name && color == label.color && icon == label.icon.as_deref();
    (!same && !name.is_empty()).then(|| Edit::LabelUpdate {
        label_id: label.label_id.clone(),
        name: name.to_owned(),
        color: color.to_owned(),
        icon: icon.map(str::to_owned),
    })
}

/// The labels of `ids`, in label order.
pub fn carried(collection: &Collection, ids: &[LabelId]) -> Vec<LabelInfo> {
    collection
        .labels
        .iter()
        .filter(|l| ids.contains(&l.label_id))
        .cloned()
        .collect()
}

/// Whether the label manager is open, provided as context by the shell: "Edit labels…"
/// anywhere opens it in the Scene objects panel.
#[derive(Clone, Copy)]
pub struct LabelManager {
    /// Whether it is open.
    pub shown: RwSignal<bool>,
    dock: Option<DockState>,
}

impl LabelManager {
    /// Closed, showing its panel through the page's dock, if it has one.
    pub fn new() -> LabelManager {
        LabelManager {
            shown: RwSignal::new(false),
            dock: use_context(),
        }
    }

    /// Opens the manager and shows the Scene objects panel that holds it.
    pub fn open(&self) {
        self.shown.set(true);
        if let Some(dock) = self.dock {
            dock.0.update(|d| d.show(PanelId::Objects));
        }
    }
}

impl Default for LabelManager {
    fn default() -> LabelManager {
        LabelManager::new()
    }
}

/// Sends `edits` as one batch through the open session.
fn send(studio: Studio, session: SessionContext, edits: Vec<Edit>) {
    if edits.is_empty() {
        return;
    }
    if let Some(s) = session.get() {
        studio.apply(&s, edits);
    }
}

/// A label's icon in its colour.
#[component]
pub fn LabelIcon(
    /// The label.
    label: LabelInfo,
    /// The icon's size.
    #[prop(default = 14)]
    size: u32,
) -> impl IntoView {
    view! {
        <span class="label-icon" style:color=label.color.clone()>
            <Icon name=label_icon(&label) size=size />
        </span>
    }
}

/// The chips of the labels a source carries: each its icon in its colour on a tint, with
/// the label's name as its tooltip.
#[component]
pub fn LabelChips(
    /// The labels, in label order.
    #[prop(into)]
    labels: Signal<Vec<LabelInfo>>,
) -> impl IntoView {
    move || {
        labels
            .get()
            .into_iter()
            .map(|label| {
                let name = label.name.clone();
                view! {
                    <span
                        class="label-chip"
                        role="img"
                        aria-label=name.clone()
                        title=name
                        style:color=label.color.clone()
                        style:background-color=tint(&label.color)
                    >
                        <Icon name=label_icon(&label) size=12 />
                    </span>
                }
            })
            .collect_view()
    }
}

/// The tag button and its popover (S20): a check row per label that puts it on the
/// source or takes it off, then "Edit labels…". `always` keeps the button shown, as the
/// Inspector's header does; on a row it shows on hover and while the popover is open.
#[component]
pub fn LabelPicker(
    /// The source the labels go on.
    source_id: SourceId,
    /// The source's name, for the button's label.
    name: String,
    /// Whether the button shows without a hover.
    #[prop(optional)]
    always: bool,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let manager = use_context::<LabelManager>();
    let open = RwSignal::new(false);
    let source = StoredValue::new(source_id);
    let rows = move || {
        studio.collection.with(|c| {
            let Some(c) = c.as_ref() else {
                return Vec::new();
            };
            let carried = c
                .source(&source.get_value())
                .map(|s| s.meta.labels.clone())
                .unwrap_or_default();
            c.labels
                .iter()
                .map(|l| (l.clone(), carried.contains(&l.label_id)))
                .collect::<Vec<_>>()
        })
    };
    let toggle = move |label: LabelId| {
        let edit = studio.collection.with_untracked(|c| {
            c.as_ref()
                .and_then(|c| toggle_edit(c, &source.get_value(), &label))
        });
        send(studio, session, edit.into_iter().collect());
    };
    view! {
        <div class="label-picker">
            <button
                type="button"
                class="tree-tool tag-button"
                class:hov=!always
                class:on=move || open.get()
                title="Labels"
                aria-label=format!("Labels for {name}")
                aria-expanded=move || open.get().to_string()
                on:click=move |_| open.update(|o| *o = !*o)
            >
                <Icon name="tag" size=14 />
            </button>
            <Show when=move || open.get()>
                <div class="menu-catcher" on:pointerdown=move |_| open.set(false)></div>
                <div
                    class="label-pop"
                    role="group"
                    aria-label="Labels"
                    on:keydown=move |event| {
                        if event.key() == "Escape" {
                            event.stop_propagation();
                            open.set(false);
                        }
                    }
                >
                    {move || {
                        let rows = rows();
                        if rows.is_empty() {
                            return view! { <p class="label-none">"No labels yet."</p> }.into_any();
                        }
                        rows.into_iter()
                            .map(|(label, on)| {
                                let id = label.label_id.clone();
                                let name = label.name.clone();
                                view! {
                                    <button
                                        type="button"
                                        class="tree-check"
                                        aria-pressed=on.to_string()
                                        on:click=move |_| toggle(id.clone())
                                    >
                                        <span class="tree-box label-box" class:on=on>{on.then_some("✓")}</span>
                                        <LabelIcon label=label />
                                        {name}
                                    </button>
                                }
                            })
                            .collect_view()
                            .into_any()
                    }}
                    {manager.map(|m| view! {
                        <button
                            type="button"
                            class="link"
                            on:click=move |_| {
                                open.set(false);
                                m.open();
                            }
                        >
                            "Edit labels…"
                        </button>
                    })}
                </div>
            </Show>
        </div>
    }
}

/// The label manager (S7): a card with a row per label (an icon button that opens the
/// icon picker, the name, ×, and the colour swatches), then "+ New label".
#[component]
pub fn LabelEditor() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let manager = expect_context::<LabelManager>();
    // The label whose icon picker is open.
    let picking = RwSignal::new(None::<LabelId>);
    let labels = move || {
        studio
            .collection
            .with(|c| c.as_ref().map(|c| c.labels.clone()).unwrap_or_default())
    };
    let update =
        move |id: &LabelId, name: Option<&str>, color: Option<&str>, icon: Option<&str>| {
            let edit = studio.collection.with_untracked(|c| {
                let label = c.as_ref()?.labels.iter().find(|l| &l.label_id == id)?;
                update_edit(label, name, color, icon)
            });
            send(studio, session, edit.into_iter().collect());
        };
    let add = move |_| {
        let Some(s) = session.get() else { return };
        let label_id = LabelId::new(studio.fresh_id(&s, "label"));
        let edit = studio
            .collection
            .with_untracked(|c| c.as_ref().map(|c| new_label_edit(c, label_id.clone())));
        if let Some(edit) = edit {
            studio.apply(&s, vec![edit]);
            picking.set(Some(label_id));
        }
    };
    let remove = move |id: LabelId| {
        let edits = studio
            .collection
            .with_untracked(|c| c.as_ref().map(|c| delete_edits(c, &id)).unwrap_or_default());
        send(studio, session, edits);
    };

    let row = move |label: LabelInfo| {
        let id = StoredValue::new(label.label_id.clone());
        let name = label.name.clone();
        let current_icon = label_icon(&label);
        let color = label.color.clone();
        let swatches = SWATCHES
            .into_iter()
            .map(|swatch| {
                let chosen = swatch.eq_ignore_ascii_case(&color);
                view! {
                    <button
                        type="button"
                        class="swatch"
                        class:on=chosen
                        aria-label=format!("Color {swatch}")
                        aria-pressed=chosen.to_string()
                        style:background-color=swatch
                        on:click=move |_| update(&id.get_value(), None, Some(swatch), None)
                    ></button>
                }
            })
            .collect_view();
        let groups = {
            let color = color.clone();
            move || {
                (picking.get().as_ref() == Some(&id.get_value())).then(|| {
                    let color = color.clone();
                    ICON_GROUPS
                        .into_iter()
                        .map(|(title, icons)| {
                            let color = color.clone();
                            let buttons = icons
                                .iter()
                                .map(|icon| {
                                    let icon = *icon;
                                    let on = icon == current_icon;
                                    view! {
                                        <button
                                            type="button"
                                            class="icon-pick"
                                            class:on=on
                                            aria-label=icon
                                            title=icon
                                            aria-pressed=on.to_string()
                                            style:border-color=on.then(|| color.clone())
                                            style:background-color=on.then(|| tint(&color))
                                            on:click=move |_| update(&id.get_value(), None, None, Some(icon))
                                        >
                                            <Icon name=icon size=16 />
                                        </button>
                                    }
                                })
                                .collect_view();
                            view! {
                                <div class="icon-group">
                                    <span class="icon-group-title">{title}</span>
                                    <div class="icon-grid">{buttons}</div>
                                </div>
                            }
                        })
                        .collect_view()
                })
            }
        };
        view! {
            <div class="label-edit">
                <div class="label-edit-head">
                    <button
                        type="button"
                        class="label-icon-button"
                        title="Change icon"
                        aria-label=format!("Change icon for {name}")
                        aria-expanded=move || (picking.get().as_ref() == Some(&id.get_value())).to_string()
                        style:border-color=color.clone()
                        style:color=color.clone()
                        on:click=move |_| {
                            let mine = id.get_value();
                            picking.update(|p| {
                                *p = if p.as_ref() == Some(&mine) { None } else { Some(mine) };
                            });
                        }
                    >
                        <Icon name=current_icon size=16 />
                    </button>
                    <input
                        type="text"
                        aria-label="Label name"
                        prop:value=name.clone()
                        on:change=move |event| {
                            update(&id.get_value(), Some(&event_target_value(&event)), None, None);
                        }
                    />
                    <button
                        type="button"
                        class="label-delete"
                        title="Delete label"
                        aria-label=format!("Delete label {name}")
                        on:click=move |_| remove(id.get_value())
                    >
                        <Icon name="close" size=14 />
                    </button>
                </div>
                <div class="swatches">{swatches}</div>
                {groups}
            </div>
        }
    };

    view! {
        <div class="label-manager" role="group" aria-label="Labels">
            <div class="label-manager-head">
                <span class="section-title">"Labels"</span>
                <button type="button" class="link" on:click=move |_| manager.shown.set(false)>"Done"</button>
            </div>
            <For
                each=labels
                key=|l| (l.label_id.clone(), l.name.clone(), l.color.clone(), l.icon.clone())
                children=row
            />
            <button type="button" class="link new-label" on:click=add>"+ New label"</button>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{SourceInfo, SourceKind};

    fn label(id: &str, name: &str) -> LabelInfo {
        LabelInfo {
            label_id: id.into(),
            name: name.into(),
            color: "#E8A33D".into(),
            icon: Some("flag".into()),
        }
    }

    fn source(id: &str, labels: &[&str], favourite: bool) -> SourceInfo {
        SourceInfo {
            source_id: id.into(),
            name: id.into(),
            kind: SourceKind::Builtin {
                kind: "color".into(),
            },
            settings: Default::default(),
            duplicated_from: None,
            audio: None,
            filters: Default::default(),
            meta: SourceMeta {
                labels: labels.iter().map(|l| LabelId::new(*l)).collect(),
                favourite,
            },
        }
    }

    fn collection() -> Collection {
        Collection {
            sources: vec![
                source("a", &["l1"], true),
                source("b", &["l1", "l2"], false),
                source("c", &[], false),
            ],
            labels: vec![label("l1", "Intro / outro"), label("l2", "Gameplay")],
            ..Collection::default()
        }
    }

    #[test]
    fn a_label_goes_on_in_id_order_and_comes_off() {
        let meta = SourceMeta {
            labels: vec!["l3".into()],
            favourite: true,
        };
        let on = with_label(&meta, &"l1".into(), true);
        assert_eq!(on.labels, [LabelId::new("l1"), LabelId::new("l3")]);
        assert!(on.favourite);
        assert_eq!(
            with_label(&on, &"l3".into(), false).labels,
            [LabelId::new("l1")]
        );
        // Putting on a label it carries keeps one copy.
        assert_eq!(with_label(&on, &"l1".into(), true).labels.len(), 2);
    }

    #[test]
    fn the_popover_toggles_the_sources_label() {
        let c = collection();
        let Some(Edit::SourceSetMeta { meta, .. }) = toggle_edit(&c, &"c".into(), &"l2".into())
        else {
            panic!("a set_meta");
        };
        assert_eq!(meta.labels, [LabelId::new("l2")]);
        let Some(Edit::SourceSetMeta { meta, .. }) = toggle_edit(&c, &"b".into(), &"l1".into())
        else {
            panic!("a set_meta");
        };
        assert_eq!(meta.labels, [LabelId::new("l2")]);
        assert!(toggle_edit(&c, &"gone".into(), &"l1".into()).is_none());
    }

    #[test]
    fn deleting_a_label_takes_it_off_its_sources_first() {
        let edits = delete_edits(&collection(), &"l1".into());
        assert_eq!(edits.len(), 3);
        assert!(
            matches!(&edits[0], Edit::SourceSetMeta { source_id, meta } if source_id.as_str() == "a" && meta.labels.is_empty() && meta.favourite)
        );
        assert!(
            matches!(&edits[1], Edit::SourceSetMeta { source_id, meta } if source_id.as_str() == "b" && meta.labels == [LabelId::new("l2")])
        );
        assert!(matches!(&edits[2], Edit::LabelDelete { label_id } if label_id.as_str() == "l1"));
    }

    #[test]
    fn a_new_label_takes_the_next_swatch() {
        let Edit::LabelCreate {
            name, color, icon, ..
        } = new_label_edit(&collection(), "l9".into())
        else {
            panic!("a label.create");
        };
        assert_eq!(
            (name.as_str(), color.as_str(), icon.as_deref()),
            ("New label", SWATCHES[2], Some("tag"))
        );
    }

    #[test]
    fn an_update_keeps_what_it_doesnt_change() {
        let l = label("l1", "Intro");
        let Some(Edit::LabelUpdate {
            name, color, icon, ..
        }) = update_edit(&l, None, Some("#4C8DFF"), None)
        else {
            panic!("a label.update");
        };
        assert_eq!(
            (name.as_str(), color.as_str(), icon.as_deref()),
            ("Intro", "#4C8DFF", Some("flag"))
        );
        assert!(
            update_edit(&l, Some("  Intro "), None, None).is_none(),
            "the same name"
        );
        assert!(
            update_edit(&l, Some(" "), None, None).is_none(),
            "a blank name"
        );
        assert!(
            update_edit(&l, None, None, Some("flag")).is_none(),
            "the same icon"
        );
    }

    #[test]
    fn carried_labels_come_in_label_order() {
        let c = collection();
        let names: Vec<String> = carried(&c, &["l2".into(), "l1".into()])
            .into_iter()
            .map(|l| l.name)
            .collect();
        assert_eq!(names, ["Intro / outro", "Gameplay"]);
        assert_eq!(
            label_icon(&LabelInfo {
                icon: None,
                ..label("x", "x")
            }),
            "tag"
        );
        assert_eq!(tint("#E8A33D"), "#E8A33D38");
    }
}
