//! Mapping a control onto an item's placement (UI-FEATURES VC-05): the Inspector's
//! position, size, rotation and opacity fields as targets of the Map control dialog and
//! of mapping mode. A mapping is a motion modifier on the item that reads the source and
//! adds its offset to the property (ADR-0099, ADR-0171), one per source, so several
//! sources can move one property and one source many.
//!
//! [`ItemMapper`] is made once per item shown. It gives each field its [`MapMark`],
//! takes the right-click and the mapping-mode drag that name one of its fields, and
//! draws the field's menu.
//!
//! Docs: docs/UI-FEATURES.md#automation, docs/adr/0171-live-data-from-a-daw-drives-scenes.md

use crate::app::SessionContext;
use crate::live_sources::reader_name;
use crate::panels::learn::ControlMenuAt;
use crate::panels::map_control::{
    MapControlDialog, MapControlRequest, ends, picked, same_source, with_ends,
};
use crate::panels::map_target::{ItemUnit, MapMark, MapTarget, offset_after};
use crate::panels::mapping_mode::MappingMode;
use crate::panels::motion::property_name;
use crate::store::Studio;
use jstream_control::protocol::{ItemId, MotionProperty, SceneId};
use leptos::prelude::*;

/// The property a field's mark names, when the mark is one of `item`'s.
pub fn property_of(id: &str, item: &ItemId) -> Option<MotionProperty> {
    let name = id
        .strip_prefix("item:")?
        .strip_prefix(item.as_str())?
        .strip_prefix(':')?;
    let property: MotionProperty = serde_json::from_str(&format!("\"{name}\"")).ok()?;
    ItemUnit::of(property).map(|_| property)
}

/// The mapping targets of one item's fields.
#[derive(Clone, Copy)]
pub struct ItemMapper {
    ids: StoredValue<(SceneId, ItemId)>,
    studio: Studio,
    mode: Option<MappingMode>,
    /// The offset a drag under way has reached, on one property.
    preview: RwSignal<Option<(MotionProperty, (f32, f32))>>,
    /// The open menu: its property and where the pointer was.
    menu: RwSignal<Option<(MotionProperty, f64, f64)>>,
}

impl ItemMapper {
    /// The mapper for `item` as `scene` shows it. Call it in the component that draws
    /// the item's fields: it lives as long as that component.
    pub fn new(scene: SceneId, item: ItemId) -> Self {
        let studio = expect_context::<Studio>();
        let session = expect_context::<SessionContext>();
        let mode = use_context::<MappingMode>();
        let mapper = ItemMapper {
            ids: StoredValue::new((scene, item.clone())),
            studio,
            mode,
            preview: RwSignal::new(None),
            menu: RwSignal::new(None),
        };
        // A right-click on one of the fields opens its menu where the pointer was.
        if let Some(at) = use_context::<ControlMenuAt>() {
            let item = item.clone();
            Effect::new(move |_| {
                if let Some((id, x, y)) = at.0.get()
                    && let Some(property) = property_of(&id, &item)
                {
                    at.0.set(None);
                    mapper.menu.set(Some((property, x, y)));
                }
            });
        }
        // A mapping-mode drag on one of the fields: shown as it goes, written once.
        if let Some(mode) = mode {
            Effect::new(move |_| {
                let drag = mode.drag.get();
                let Some((drag, property)) =
                    drag.and_then(|d| property_of(&d.setting, &item).map(|p| (d, p)))
                else {
                    mapper.preview.set(None);
                    return;
                };
                let (Some(armed), Some(unit)) =
                    (mode.armed.get_untracked(), ItemUnit::of(property))
                else {
                    return;
                };
                let target = mapper.target(property);
                let existing = studio.collection.with_untracked(|c| {
                    c.as_ref()
                        .and_then(|c| target.mapping_from(c, &armed.reader))
                });
                let before = existing.as_ref().and_then(|m| ends(&m.input));
                let offset = offset_after(before, unit, drag.amount);
                if !drag.done {
                    mapper.preview.set(Some((property, offset)));
                    return;
                }
                mode.drag.set(None);
                mapper.preview.set(None);
                // A drag that ends where it began on an unmapped property maps nothing.
                if existing.is_none() && offset.0 == offset.1 {
                    return;
                }
                let input = with_ends(
                    picked(
                        armed.reader,
                        existing.as_ref().map(|m| &m.input),
                        (0.0, 0.0),
                    ),
                    offset.0,
                    offset.1,
                );
                let amount = existing.and_then(|m| m.amount);
                let edits = studio.collection.with_untracked(|c| {
                    c.as_ref()
                        .map(|c| target.set(c, input, amount, None))
                        .unwrap_or_default()
                });
                if let (false, Some(s)) = (edits.is_empty(), session.0.get_value()) {
                    studio.apply(&s, edits);
                }
            });
        }
        mapper
    }

    /// The target that is `property` of this item.
    pub fn target(&self, property: MotionProperty) -> MapTarget {
        let (scene, item) = self.ids.get_value();
        MapTarget::Item {
            scene,
            item,
            property,
        }
    }

    /// The mark for the field that shows `property`. Its tag is the offset of the drag
    /// under way; in mapping mode, the armed source's offset; otherwise the one
    /// mapping's offset, or how many there are.
    pub fn mark(&self, property: MotionProperty) -> MapMark {
        let mapper = *self;
        let target = self.target(property);
        let id = target.id();
        let amount = Signal::derive(move || {
            let unit = ItemUnit::of(property)?;
            let offset = |(from, to): (f32, f32)| unit.label(f64::from(to) - f64::from(from));
            if let Some((p, reach)) = mapper.preview.get()
                && p == property
            {
                return Some(offset(reach));
            }
            let mappings = mapper
                .studio
                .collection
                .with(|c| c.as_ref().map(|c| target.mappings(c)).unwrap_or_default());
            if let Some(armed) = mapper.mode.and_then(|m| m.armed.get()) {
                return mappings
                    .iter()
                    .find(|m| same_source(&m.input, &armed.reader))
                    .and_then(|m| ends(&m.input))
                    .map(offset);
            }
            match &mappings[..] {
                [] => None,
                [one] => ends(&one.input).map(offset),
                many => Some(format!("{} maps", many.len())),
            }
        });
        MapMark { id, amount }
    }

    /// The menu of the field that was right-clicked: Map control…, then each mapping on
    /// the property with its offset, to remove. Draw it once, beside the fields.
    pub fn menu(self) -> impl IntoView {
        let session = expect_context::<SessionContext>();
        let dialog = use_context::<MapControlDialog>();
        let studio = self.studio;
        move || {
            let (property, x, y) = self.menu.get()?;
            let unit = ItemUnit::of(property)?;
            let target = self.target(property);
            let close = move || self.menu.set(None);
            let name = property_name(property);
            let open_dialog = {
                let target = target.clone();
                move |_| {
                    close();
                    if let Some(dialog) = dialog {
                        dialog.0.set(Some(MapControlRequest {
                            target: target.clone(),
                            range: unit.first_ends(property),
                            limits: (-unit.span(), unit.span()),
                            label: name.to_owned(),
                            current: None,
                        }));
                    }
                }
            };
            let mappings = studio.collection.with(|c| {
                c.as_ref()
                    .map(|c| (target.mappings(c), c))
                    .map(|(all, c)| {
                        all.into_iter()
                            .map(|m| {
                                let source = reader_name(Some(c), &m.input);
                                let offset = target
                                    .amount_label(&m.input, (0.0, 0.0))
                                    .unwrap_or_default();
                                (m.key, source, offset)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            let rows = mappings
                .into_iter()
                .map(|(key, source, offset)| {
                    let target = target.clone();
                    let remove = move |_| {
                        close();
                        let edits = studio.collection.with_untracked(|c| {
                            c.as_ref()
                                .map(|c| target.clear(c, &key))
                                .unwrap_or_default()
                        });
                        if let (false, Some(s)) = (edits.is_empty(), session.0.get_value()) {
                            studio.apply(&s, edits);
                        }
                    };
                    view! {
                        <button type="button" class="menuitem" role="menuitem" on:click=remove>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">"Remove " {source}</span>
                            <span class="hint">{offset}</span>
                        </button>
                    }
                })
                .collect_view();
            let (w, h) = window_size();
            let left = (x - 20.0).min(w - 296.0).max(4.0);
            let top = if y + 200.0 > h {
                (y - 200.0).max(4.0)
            } else {
                y + 4.0
            };
            Some(view! {
                <div class="ctx-catcher" on:pointerdown=move |_| close()
                    on:contextmenu=move |e: leptos::ev::MouseEvent| { e.prevent_default(); close(); }></div>
                <div class="context-menu control-menu" role="menu" aria-label="Control"
                    style:left=format!("{left}px") style:top=format!("{top}px")
                    on:keydown=move |e: leptos::ev::KeyboardEvent| {
                        if e.key() == "Escape" {
                            e.prevent_default();
                            e.stop_propagation();
                            close();
                        }
                    }>
                    <div class="ctx-head">{name}</div>
                    <button type="button" class="menuitem" role="menuitem" on:click=open_dialog>
                        <span class="ctx-check"></span>
                        <span class="ctx-label">"Map control…"</span>
                    </button>
                    {rows}
                </div>
            })
        }
    }
}

/// The window's inner size, in CSS pixels.
fn window_size() -> (f64, f64) {
    let n = |v: Result<leptos::wasm_bindgen::JsValue, leptos::wasm_bindgen::JsValue>| {
        v.ok().and_then(|v| v.as_f64())
    };
    web_sys::window()
        .map(|w| {
            (
                n(w.inner_width()).unwrap_or(1600.0),
                n(w.inner_height()).unwrap_or(1000.0),
            )
        })
        .unwrap_or((1600.0, 1000.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_names_a_property_of_its_own_item_only() {
        let item = ItemId::new("itm_1");
        let target = MapTarget::Item {
            scene: SceneId::new("scn_1"),
            item: item.clone(),
            property: MotionProperty::Opacity,
        };
        assert_eq!(
            property_of(&target.id(), &item),
            Some(MotionProperty::Opacity)
        );
        assert_eq!(property_of(&target.id(), &ItemId::new("itm_2")), None);
        assert_eq!(property_of("item:itm_1:effect", &item), None);
        assert_eq!(property_of("learn_0123", &item), None);
        // An ID that only starts with this item's is another item's.
        assert_eq!(property_of("item:itm_10:x", &item), None);
    }
}
