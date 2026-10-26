//! The dock's model: which panels sit in which of the workspace's eight zones, grouped
//! and tabbed, how big each zone is, and the saved layouts. Every change the workspace
//! and the header's View and Layout menus make goes through [`Dock`], which is plain data
//! so it is tested natively; `workspace.rs` draws it.
//!
//! Docs: docs/UI-FEATURES.md#docking

use serde_json::{Value, json};

/// The name of the default layout.
pub const EDITING: &str = "Editing";
/// The name of the layout for streaming.
pub const LIVE: &str = "Live";
/// What a layout's name gets once it is changed.
const MODIFIED: &str = " (modified)";

/// One of the workspace's dock zones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Zone {
    /// The outer left column, the full height under the header.
    OuterLeft,
    /// The full-width top, between the outer columns.
    Top,
    /// The left sidebar.
    Left,
    /// Above the canvas.
    CanvasTop,
    /// Below the canvas.
    CanvasBottom,
    /// The right sidebar.
    Right,
    /// The full-width bottom, between the outer columns.
    Bottom,
    /// The outer right column.
    OuterRight,
}

impl Zone {
    /// Every zone, in the order the workspace lays them out.
    pub const ALL: [Zone; 8] = [
        Zone::OuterLeft,
        Zone::Top,
        Zone::Left,
        Zone::CanvasTop,
        Zone::CanvasBottom,
        Zone::Right,
        Zone::Bottom,
        Zone::OuterRight,
    ];

    /// The zone's index in [`Zone::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// The zone's key in a stored layout.
    pub fn key(self) -> &'static str {
        match self {
            Zone::OuterLeft => "oleft",
            Zone::Top => "top",
            Zone::Left => "left",
            Zone::CanvasTop => "ctop",
            Zone::CanvasBottom => "cbottom",
            Zone::Right => "right",
            Zone::Bottom => "bottom",
            Zone::OuterRight => "oright",
        }
    }

    /// Whether the zone is a column, stacking its groups top to bottom. The others place
    /// them side by side.
    pub fn vertical(self) -> bool {
        matches!(
            self,
            Zone::OuterLeft | Zone::Left | Zone::Right | Zone::OuterRight
        )
    }

    /// The smallest and largest size, in pixels: a column's width or a row's height.
    pub fn limits(self) -> (f64, f64) {
        match self {
            Zone::OuterLeft | Zone::OuterRight | Zone::Left => (180.0, 560.0),
            Zone::Right => (200.0, 560.0),
            Zone::Top => (90.0, 400.0),
            Zone::Bottom => (90.0, 440.0),
            Zone::CanvasTop | Zone::CanvasBottom => (80.0, 400.0),
        }
    }

    /// The zone's accessible name.
    pub fn label(self) -> &'static str {
        match self {
            Zone::OuterLeft => "Outer left column",
            Zone::Top => "Top dock",
            Zone::Left => "Left sidebar",
            Zone::CanvasTop => "Dock above the canvas",
            Zone::CanvasBottom => "Dock below the canvas",
            Zone::Right => "Right sidebar",
            Zone::Bottom => "Bottom dock",
            Zone::OuterRight => "Outer right column",
        }
    }

    /// The accessible name of the zone's splitter.
    pub fn splitter_label(self) -> &'static str {
        match self {
            Zone::OuterLeft => "Resize outer left column",
            Zone::Top => "Resize top dock",
            Zone::Left => "Resize left sidebar",
            Zone::CanvasTop => "Resize dock above the canvas",
            Zone::CanvasBottom => "Resize dock below the canvas",
            Zone::Right => "Resize right sidebar",
            Zone::Bottom => "Resize bottom dock",
            Zone::OuterRight => "Resize outer right column",
        }
    }

    /// What an empty zone says while a panel is dragged.
    pub fn placeholder(self) -> &'static str {
        match self {
            Zone::OuterLeft => "Drop here: outer left",
            Zone::Top => "Drop here: full-width top",
            Zone::Left => "Drop here: left",
            Zone::CanvasTop => "Drop here: above the canvas",
            Zone::CanvasBottom => "Drop here: below the canvas",
            Zone::Right => "Drop here: right",
            Zone::Bottom => "Drop here: full-width bottom",
            Zone::OuterRight => "Drop here: outer right",
        }
    }

    /// Whether the splitter comes after the zone, which it does for zones left of or
    /// above the canvas; it always sits on the canvas side.
    pub fn splitter_after(self) -> bool {
        matches!(
            self,
            Zone::OuterLeft | Zone::Top | Zone::Left | Zone::CanvasTop
        )
    }

    /// How a splitter drag of `(dx, dy)` pixels changes the zone's size: it grows
    /// towards the canvas.
    pub fn grow(self, dx: f64, dy: f64) -> f64 {
        match self {
            Zone::OuterLeft | Zone::Left => dx,
            Zone::Right | Zone::OuterRight => -dx,
            Zone::Top | Zone::CanvasTop => dy,
            Zone::CanvasBottom | Zone::Bottom => -dy,
        }
    }

    /// The zone a stored key names.
    fn from_key(key: &str) -> Option<Zone> {
        Zone::ALL.into_iter().find(|zone| zone.key() == key)
    }
}

/// A panel the workspace can dock. Each is shown at most once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PanelId {
    /// The scene list.
    Scenes,
    /// The selected scene's items.
    Objects,
    /// The selected item's properties.
    Inspector,
    /// The audio mixer.
    Mixer,
    /// The outputs.
    Outputs,
    /// Studio mode (preview, program, Transition and Cut) and the transition presets.
    Transitions,
    /// Align, distribute and arrange the selected items.
    Transform,
    /// The selected item's source's filter chains.
    Filters,
    /// A YouTube broadcast's live chat.
    Chat,
    /// A YouTube broadcast's stream stats.
    Stats,
    /// What agents changed.
    Agent,
}

impl PanelId {
    /// Every panel, in the View menu's order.
    pub const ALL: [PanelId; 11] = [
        PanelId::Scenes,
        PanelId::Objects,
        PanelId::Inspector,
        PanelId::Transform,
        PanelId::Filters,
        PanelId::Chat,
        PanelId::Stats,
        PanelId::Mixer,
        PanelId::Outputs,
        PanelId::Agent,
        PanelId::Transitions,
    ];

    /// The panel's title, on its tab and in the View menu.
    pub fn title(self) -> &'static str {
        match self {
            PanelId::Scenes => "Scenes",
            PanelId::Objects => "Scene objects",
            PanelId::Inspector => "Inspector",
            PanelId::Mixer => "Audio mixer",
            PanelId::Outputs => "Outputs",
            PanelId::Transitions => "Transitions",
            PanelId::Transform => "Transform",
            PanelId::Filters => "Filters",
            PanelId::Chat => "Live chat",
            PanelId::Stats => "Stream stats",
            PanelId::Agent => "Agent activity",
        }
    }

    /// The panel's key in a stored layout, the prototype's ID.
    pub fn key(self) -> &'static str {
        match self {
            PanelId::Scenes => "scenes",
            PanelId::Objects => "objects",
            PanelId::Inspector => "inspector",
            PanelId::Mixer => "mixer",
            PanelId::Outputs => "outputs",
            PanelId::Transitions => "transitions",
            PanelId::Transform => "transform",
            PanelId::Filters => "filters",
            PanelId::Chat => "chat",
            PanelId::Stats => "ytstats",
            PanelId::Agent => "agent",
        }
    }

    /// Where the panel goes when it is added from the View menu.
    pub fn home(self) -> Zone {
        match self {
            PanelId::Scenes | PanelId::Objects => Zone::Left,
            PanelId::Inspector
            | PanelId::Transform
            | PanelId::Filters
            | PanelId::Chat
            | PanelId::Stats
            | PanelId::Agent => Zone::Right,
            PanelId::Mixer | PanelId::Outputs | PanelId::Transitions => Zone::Bottom,
        }
    }

    /// The panel a stored key names. Keys of panels this UI doesn't have yet name none.
    fn from_key(key: &str) -> Option<PanelId> {
        PanelId::ALL.into_iter().find(|panel| panel.key() == key)
    }
}

/// Panels in one place: a lone panel with a title, or tabs.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// Unique within the dock, so the workspace can key its views.
    pub id: u32,
    /// The panels, in tab order. Never empty.
    pub tabs: Vec<PanelId>,
    /// The one shown.
    pub active: PanelId,
    /// Its share of the zone, against the other groups'.
    pub weight: f64,
    /// Whether only its title shows.
    pub collapsed: bool,
}

/// Where in a zone's row of groups something is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// The zone.
    pub zone: Zone,
    /// The group's index in it.
    pub index: usize,
}

/// Where a dragged panel lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// As a new tab of group `id`.
    Tabs(u32),
    /// As a new group before group `id`.
    Before(u32),
    /// As a new group after group `id`.
    After(u32),
    /// As a new group at the end of a zone.
    End(Zone),
}

/// Which panels sit where, and how big each zone is.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrangement {
    /// Each zone's groups, indexed by [`Zone::index`].
    pub zones: [Vec<Group>; 8],
    /// Each zone's size in pixels, indexed by [`Zone::index`].
    pub sizes: [f64; 8],
}

/// A saved layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Its name in the Layout menu.
    pub name: String,
    /// What it holds. Group IDs are renumbered when it is applied.
    pub arrangement: Arrangement,
}

/// The default sizes: outer left, top, left, above and below the canvas, right, bottom,
/// outer right.
const SIZES: [f64; 8] = [260.0, 140.0, 300.0, 140.0, 170.0, 340.0, 190.0, 280.0];

/// The default layout: Scenes over Scene objects on the left, the inspector over the
/// Filters and Transform panels (one group, Filters shown) on the right, and the mixer and
/// the outputs (with Transitions as a second tab) along the bottom.
pub fn editing() -> Arrangement {
    use PanelId::*;
    arrangement(
        SIZES,
        &[
            (Zone::Left, vec![(vec![Scenes], 1.0), (vec![Objects], 1.0)]),
            (
                Zone::Right,
                vec![(vec![Inspector], 2.0), (vec![Filters, Transform], 1.0)],
            ),
            (
                Zone::Bottom,
                vec![(vec![Mixer], 1.0), (vec![Outputs, Transitions], 1.0)],
            ),
        ],
    )
}

/// The layout for streaming: Scenes on the left, the mixer and Transitions under the
/// canvas, and the outputs on the right.
pub fn live() -> Arrangement {
    use PanelId::*;
    let mut sizes = SIZES;
    sizes[Zone::Left.index()] = 260.0;
    sizes[Zone::CanvasBottom.index()] = 180.0;
    arrangement(
        sizes,
        &[
            (Zone::Left, vec![(vec![Scenes], 1.0)]),
            (
                Zone::CanvasBottom,
                vec![(vec![Mixer], 1.0), (vec![Transitions], 1.0)],
            ),
            (Zone::Right, vec![(vec![Outputs], 1.0)]),
        ],
    )
}

/// A zone's groups in a built-in layout: each group's tabs, the first active, and its
/// weight.
type Groups = Vec<(Vec<PanelId>, f64)>;

/// An arrangement of `groups` per zone. IDs are placeholders until [`Dock::adopt`]
/// numbers them.
fn arrangement(sizes: [f64; 8], groups: &[(Zone, Groups)]) -> Arrangement {
    let mut zones: [Vec<Group>; 8] = Default::default();
    for (zone, list) in groups {
        zones[zone.index()] = list
            .iter()
            .map(|(tabs, weight)| Group {
                id: 0,
                tabs: tabs.clone(),
                active: tabs[0],
                weight: *weight,
                collapsed: false,
            })
            .collect();
    }
    Arrangement { zones, sizes }
}

/// The dock: the arrangement on screen, its layout's name and the saved layouts.
#[derive(Clone, Debug, PartialEq)]
pub struct Dock {
    /// What is on screen.
    pub arrangement: Arrangement,
    /// The layout's name, with " (modified)" once it was changed.
    pub current: String,
    /// The saved layouts, Editing and Live first.
    pub saved: Vec<Layout>,
    /// The next group ID.
    next_id: u32,
}

impl Default for Dock {
    fn default() -> Self {
        Dock::new()
    }
}

impl Dock {
    /// The Editing layout on screen, with Editing and Live saved.
    pub fn new() -> Dock {
        let mut dock = Dock {
            arrangement: arrangement(SIZES, &[]),
            current: EDITING.into(),
            saved: vec![
                Layout {
                    name: EDITING.into(),
                    arrangement: editing(),
                },
                Layout {
                    name: LIVE.into(),
                    arrangement: live(),
                },
            ],
            next_id: 100,
        };
        dock.adopt(editing());
        dock
    }

    /// Puts `arrangement` on screen with fresh group IDs.
    fn adopt(&mut self, mut arrangement: Arrangement) {
        for group in arrangement.zones.iter_mut().flatten() {
            group.id = self.next_id;
            self.next_id += 1;
        }
        self.arrangement = arrangement;
    }

    /// The groups in `zone`.
    pub fn groups(&self, zone: Zone) -> &[Group] {
        &self.arrangement.zones[zone.index()]
    }

    /// `zone`'s size in pixels.
    pub fn size(&self, zone: Zone) -> f64 {
        self.arrangement.sizes[zone.index()]
    }

    /// Group `id`, if it exists.
    pub fn group(&self, id: u32) -> Option<&Group> {
        self.arrangement.zones.iter().flatten().find(|g| g.id == id)
    }

    /// Where group `id` is.
    pub fn place_of(&self, id: u32) -> Option<Place> {
        Zone::ALL.into_iter().find_map(|zone| {
            self.groups(zone)
                .iter()
                .position(|g| g.id == id)
                .map(|index| Place { zone, index })
        })
    }

    /// The group showing `panel`, if it is docked.
    pub fn group_of(&self, panel: PanelId) -> Option<&Group> {
        self.arrangement
            .zones
            .iter()
            .flatten()
            .find(|g| g.tabs.contains(&panel))
    }

    /// Whether `panel` is docked anywhere.
    pub fn shown(&self, panel: PanelId) -> bool {
        self.group_of(panel).is_some()
    }

    /// Appends " (modified)" to the layout's name, once.
    fn modified(&mut self) {
        if !self.current.ends_with(MODIFIED) {
            self.current.push_str(MODIFIED);
        }
    }

    /// Takes `panel` out of its group, deleting the group if it empties.
    fn take(&mut self, panel: PanelId) {
        for groups in &mut self.arrangement.zones {
            for group in groups.iter_mut() {
                if let Some(at) = group.tabs.iter().position(|&p| p == panel) {
                    group.tabs.remove(at);
                    if group.active == panel
                        && let Some(&next) = group.tabs.get(at.min(group.tabs.len().max(1) - 1))
                    {
                        group.active = next;
                    }
                }
            }
            groups.retain(|g| !g.tabs.is_empty());
        }
    }

    /// A new group holding only `panel`.
    fn lone(&mut self, panel: PanelId) -> Group {
        let id = self.next_id;
        self.next_id += 1;
        Group {
            id,
            tabs: vec![panel],
            active: panel,
            weight: 1.0,
            collapsed: false,
        }
    }

    /// Docks `panel` at `target`, from wherever it was. Dropping a lone panel on its own
    /// group, or on the tabs it is already in, changes nothing. Returns whether the
    /// layout changed.
    pub fn place(&mut self, panel: PanelId, target: Target) -> bool {
        let own = self.group_of(panel).map(|g| (g.id, g.tabs.len()));
        match (target, own) {
            (Target::Tabs(id), Some((own, _))) if id == own => {
                self.activate(panel);
                return false;
            }
            (Target::Before(id) | Target::After(id), Some((own, 1))) if id == own => {
                return false;
            }
            (Target::Tabs(id) | Target::Before(id) | Target::After(id), _)
                if self.group(id).is_none() =>
            {
                return false;
            }
            _ => {}
        }
        self.take(panel);
        match target {
            Target::Tabs(id) => {
                let Some(place) = self.place_of(id) else {
                    return false;
                };
                let group = &mut self.arrangement.zones[place.zone.index()][place.index];
                group.tabs.push(panel);
                group.active = panel;
                group.collapsed = false;
            }
            Target::Before(id) | Target::After(id) => {
                let Some(place) = self.place_of(id) else {
                    return false;
                };
                let at = place.index + usize::from(matches!(target, Target::After(_)));
                let group = self.lone(panel);
                self.arrangement.zones[place.zone.index()].insert(at, group);
            }
            Target::End(zone) => {
                let group = self.lone(panel);
                self.arrangement.zones[zone.index()].push(group);
            }
        }
        self.modified();
        true
    }

    /// Closes `panel`.
    pub fn close(&mut self, panel: PanelId) {
        if self.shown(panel) {
            self.take(panel);
            self.modified();
        }
    }

    /// The View menu's toggle: closes a shown panel, or adds a hidden one in its home
    /// zone.
    pub fn toggle(&mut self, panel: PanelId) {
        if self.shown(panel) {
            self.take(panel);
        } else {
            let group = self.lone(panel);
            self.arrangement.zones[panel.home().index()].push(group);
        }
        self.modified();
    }

    /// Brings `panel` into view: its tab is activated and its group expanded, or it is
    /// added in its home zone.
    pub fn show(&mut self, panel: PanelId) {
        if self.shown(panel) {
            self.activate(panel);
        } else {
            let group = self.lone(panel);
            self.arrangement.zones[panel.home().index()].push(group);
        }
    }

    /// Makes `panel` its group's shown tab and expands the group.
    fn activate(&mut self, panel: PanelId) {
        if let Some(group) = self
            .arrangement
            .zones
            .iter_mut()
            .flatten()
            .find(|g| g.tabs.contains(&panel))
        {
            group.active = panel;
            group.collapsed = false;
        }
    }

    /// Whether group `id` can collapse: it holds one panel and isn't alone in its zone.
    pub fn collapsible(&self, id: u32) -> bool {
        match (self.group(id), self.place_of(id)) {
            (Some(group), Some(place)) => {
                group.tabs.len() == 1 && self.groups(place.zone).len() > 1
            }
            _ => false,
        }
    }

    /// A click on `panel`'s tab in group `id`: it collapses or expands a collapsible
    /// group's shown panel, and otherwise shows the panel.
    pub fn click_tab(&mut self, id: u32, panel: PanelId) {
        let collapsible = self.collapsible(id);
        let Some(place) = self.place_of(id) else {
            return;
        };
        let group = &mut self.arrangement.zones[place.zone.index()][place.index];
        if collapsible && group.active == panel {
            group.collapsed = !group.collapsed;
        } else if group.tabs.contains(&panel) {
            group.active = panel;
            group.collapsed = false;
        }
    }

    /// Sets `zone`'s size to `from` plus a splitter drag of `(dx, dy)`, within the zone's
    /// limits.
    pub fn resize_zone(&mut self, zone: Zone, from: f64, dx: f64, dy: f64) {
        let (min, max) = zone.limits();
        self.arrangement.sizes[zone.index()] = (from + zone.grow(dx, dy)).clamp(min, max);
    }

    /// Trades weight between the groups either side of the divider before group `index`
    /// in `zone`, from their weights `from` at the drag's start, for a drag of `delta`
    /// pixels along a zone `length` pixels long. Each keeps at least 70 pixels' worth.
    pub fn resize_between(
        &mut self,
        zone: Zone,
        index: usize,
        from: (f64, f64),
        delta: f64,
        length: f64,
    ) {
        let groups = &mut self.arrangement.zones[zone.index()];
        if index == 0 || index >= groups.len() || length <= 40.0 {
            return;
        }
        let total: f64 = groups.iter().map(|g| g.weight).sum();
        let least = total * 70.0 / length;
        let pair = from.0 + from.1;
        if pair < 2.0 * least {
            return;
        }
        let before = (from.0 + delta * total / (length - 40.0)).clamp(least, pair - least);
        groups[index - 1].weight = before;
        groups[index].weight = pair - before;
    }

    /// Marks the layout changed after a resize ends.
    pub fn resized(&mut self) {
        self.modified();
    }

    /// Saves what is on screen as "My layout N" and makes it current.
    pub fn save_as_new(&mut self) {
        let name = format!("My layout {}", self.saved.len().saturating_sub(1));
        let mut arrangement = self.arrangement.clone();
        for group in arrangement.zones.iter_mut().flatten() {
            group.id = 0;
        }
        self.saved.push(Layout {
            name: name.clone(),
            arrangement,
        });
        self.current = name;
    }

    /// Puts the saved layout `name` on screen.
    pub fn apply(&mut self, name: &str) {
        if let Some(layout) = self.saved.iter().find(|l| l.name == name).cloned() {
            self.adopt(layout.arrangement);
            self.current = layout.name;
        }
    }

    /// Puts the Editing layout's defaults back on screen.
    pub fn reset(&mut self) {
        self.adopt(editing());
        self.current = EDITING.into();
    }

    /// The dock as JSON, for the browser's storage.
    pub fn to_json(&self) -> String {
        json!({
            "version": 1,
            "current": self.current,
            "arrangement": arrangement_json(&self.arrangement),
            "saved": self.saved.iter().map(|l| json!({
                "name": l.name,
                "arrangement": arrangement_json(&l.arrangement),
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// The dock [`Dock::to_json`] stored, if `text` is one. Panels this UI doesn't know
    /// are dropped, a panel docked twice keeps its first place, and sizes are brought
    /// within limits.
    pub fn from_json(text: &str) -> Option<Dock> {
        let value: Value = serde_json::from_str(text).ok()?;
        if value["version"] != 1 {
            return None;
        }
        let mut dock = Dock::new();
        let arrangement = parse_arrangement(&value["arrangement"])?;
        let mut saved = Vec::new();
        for layout in value["saved"].as_array()? {
            saved.push(Layout {
                name: layout["name"].as_str()?.to_owned(),
                arrangement: parse_arrangement(&layout["arrangement"])?,
            });
        }
        if saved.is_empty() {
            return None;
        }
        dock.saved = saved;
        dock.adopt(arrangement);
        dock.current = value["current"].as_str()?.to_owned();
        Some(dock)
    }
}

/// An arrangement as JSON.
fn arrangement_json(arrangement: &Arrangement) -> Value {
    let mut zones = serde_json::Map::new();
    let mut sizes = serde_json::Map::new();
    for zone in Zone::ALL {
        let groups = &arrangement.zones[zone.index()];
        zones.insert(
            zone.key().into(),
            groups
                .iter()
                .map(|g| {
                    json!({
                        "tabs": g.tabs.iter().map(|p| p.key()).collect::<Vec<_>>(),
                        "active": g.active.key(),
                        "weight": g.weight,
                        "collapsed": g.collapsed,
                    })
                })
                .collect(),
        );
        sizes.insert(zone.key().into(), json!(arrangement.sizes[zone.index()]));
    }
    json!({ "zones": zones, "sizes": sizes })
}

/// The arrangement `value` holds, if it is one.
fn parse_arrangement(value: &Value) -> Option<Arrangement> {
    let mut parsed = arrangement(SIZES, &[]);
    let mut seen = Vec::new();
    for (key, groups) in value["zones"].as_object()? {
        let Some(zone) = Zone::from_key(key) else {
            continue;
        };
        for group in groups.as_array()? {
            let tabs: Vec<PanelId> = group["tabs"]
                .as_array()?
                .iter()
                .filter_map(|t| t.as_str().and_then(PanelId::from_key))
                .filter(|p| !seen.contains(p))
                .collect();
            let Some(&first) = tabs.first() else {
                continue;
            };
            seen.extend(&tabs);
            let active = group["active"]
                .as_str()
                .and_then(PanelId::from_key)
                .filter(|p| tabs.contains(p))
                .unwrap_or(first);
            let weight = group["weight"]
                .as_f64()
                .filter(|w| w.is_finite() && *w > 0.0)
                .unwrap_or(1.0);
            parsed.zones[zone.index()].push(Group {
                id: 0,
                tabs,
                active,
                weight,
                collapsed: group["collapsed"].as_bool().unwrap_or(false),
            });
        }
    }
    for zone in Zone::ALL {
        if let Some(size) = value["sizes"][zone.key()].as_f64() {
            let (min, max) = zone.limits();
            if size.is_finite() {
                parsed.sizes[zone.index()] = size.clamp(min, max);
            }
        }
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panels in each non-empty zone, by group.
    fn shape(dock: &Dock) -> Vec<(Zone, Vec<Vec<PanelId>>)> {
        Zone::ALL
            .into_iter()
            .filter(|&z| !dock.groups(z).is_empty())
            .map(|z| (z, dock.groups(z).iter().map(|g| g.tabs.clone()).collect()))
            .collect()
    }

    fn id_of(dock: &Dock, panel: PanelId) -> u32 {
        dock.group_of(panel).expect("docked").id
    }

    use PanelId::*;

    #[test]
    fn editing_is_the_default_and_unmodified() {
        let dock = Dock::new();
        assert_eq!(dock.current, "Editing");
        assert_eq!(
            shape(&dock),
            vec![
                (Zone::Left, vec![vec![Scenes], vec![Objects]]),
                (Zone::Right, vec![vec![Inspector], vec![Filters, Transform]]),
                (Zone::Bottom, vec![vec![Mixer], vec![Outputs, Transitions]]),
            ]
        );
        assert_eq!(dock.size(Zone::Left), 300.0);
        assert_eq!(dock.size(Zone::Right), 340.0);
        assert_eq!(dock.size(Zone::Bottom), 190.0);
        let names: Vec<_> = dock.saved.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Editing", "Live"]);
    }

    #[test]
    fn dropping_on_tabs_groups_and_activates() {
        let mut dock = Dock::new();
        let mixer = id_of(&dock, Mixer);
        assert!(dock.place(Outputs, Target::Tabs(mixer)));
        let group = dock.group(mixer).expect("group");
        assert_eq!(group.tabs, [Mixer, Outputs]);
        assert_eq!(group.active, Outputs);
        // Transitions stays behind in the group Outputs left.
        assert_eq!(dock.groups(Zone::Bottom).len(), 2);
        assert_eq!(dock.groups(Zone::Bottom)[1].tabs, [Transitions]);
        assert_eq!(dock.current, "Editing (modified)");
        // Only once.
        dock.close(Scenes);
        assert_eq!(dock.current, "Editing (modified)");
    }

    #[test]
    fn dropping_on_a_half_splits_before_or_after() {
        let mut dock = Dock::new();
        let inspector = id_of(&dock, Inspector);
        assert!(dock.place(Scenes, Target::Before(inspector)));
        assert!(dock.place(Mixer, Target::After(inspector)));
        assert_eq!(
            shape(&dock),
            vec![
                (Zone::Left, vec![vec![Objects]]),
                (
                    Zone::Right,
                    vec![
                        vec![Scenes],
                        vec![Inspector],
                        vec![Mixer],
                        vec![Filters, Transform]
                    ]
                ),
                (Zone::Bottom, vec![vec![Outputs, Transitions]]),
            ]
        );
    }

    #[test]
    fn dropping_a_lone_panel_on_itself_changes_nothing() {
        let mut dock = Dock::new();
        let scenes = id_of(&dock, Scenes);
        assert!(!dock.place(Scenes, Target::Before(scenes)));
        assert!(!dock.place(Scenes, Target::After(scenes)));
        assert!(!dock.place(Scenes, Target::Tabs(scenes)));
        assert_eq!(dock, Dock::new());
    }

    #[test]
    fn dropping_on_a_zone_appends_and_empties_are_removed() {
        let mut dock = Dock::new();
        assert!(dock.place(Inspector, Target::End(Zone::OuterRight)));
        assert_eq!(dock.groups(Zone::Right).len(), 1);
        assert_eq!(dock.groups(Zone::OuterRight)[0].tabs, [Inspector]);
        assert!(dock.place(Transform, Target::End(Zone::OuterRight)));
        assert_eq!(dock.groups(Zone::Right)[0].tabs, [Filters]);
        assert!(dock.place(Filters, Target::End(Zone::OuterRight)));
        assert!(dock.groups(Zone::Right).is_empty());
    }

    #[test]
    fn leaving_a_tab_group_activates_a_neighbour() {
        let mut dock = Dock::new();
        let mixer = id_of(&dock, Mixer);
        dock.place(Outputs, Target::Tabs(mixer));
        dock.place(Scenes, Target::Tabs(mixer));
        dock.place(Scenes, Target::End(Zone::Top));
        assert_eq!(dock.group(mixer).expect("group").active, Outputs);
    }

    #[test]
    fn view_toggles_and_show_uses_home_zones() {
        let mut dock = Dock::new();
        dock.toggle(Inspector);
        assert!(!dock.shown(Inspector));
        assert_eq!(dock.current, "Editing (modified)");
        dock.toggle(Inspector);
        assert_eq!(
            dock.groups(Zone::Right).last().expect("group").tabs,
            [Inspector]
        );
        dock.close(Mixer);
        dock.show(Mixer);
        assert_eq!(
            dock.groups(Zone::Bottom).last().expect("group").tabs,
            [Mixer]
        );
    }

    #[test]
    fn a_lone_panel_collapses_only_beside_others() {
        let mut dock = Dock::new();
        let scenes = id_of(&dock, Scenes);
        assert!(dock.collapsible(scenes));
        dock.click_tab(scenes, Scenes);
        assert!(dock.group(scenes).expect("group").collapsed);
        dock.click_tab(scenes, Scenes);
        assert!(!dock.group(scenes).expect("group").collapsed);
        // Collapsing isn't a layout change.
        assert_eq!(dock.current, "Editing");
        dock.close(Transform);
        dock.close(Filters);
        let inspector = id_of(&dock, Inspector);
        assert!(!dock.collapsible(inspector));
        dock.click_tab(inspector, Inspector);
        assert!(!dock.group(inspector).expect("group").collapsed);
    }

    #[test]
    fn a_tab_click_shows_that_tab() {
        let mut dock = Dock::new();
        let mixer = id_of(&dock, Mixer);
        dock.place(Outputs, Target::Tabs(mixer));
        dock.click_tab(mixer, Mixer);
        let group = dock.group(mixer).expect("group");
        assert_eq!(group.active, Mixer);
        assert!(!group.collapsed);
    }

    #[test]
    fn zones_resize_towards_the_canvas_within_limits() {
        let mut dock = Dock::new();
        dock.resize_zone(Zone::Left, 300.0, 40.0, 0.0);
        assert_eq!(dock.size(Zone::Left), 340.0);
        dock.resize_zone(Zone::Right, 340.0, 40.0, 0.0);
        assert_eq!(dock.size(Zone::Right), 300.0);
        dock.resize_zone(Zone::Bottom, 190.0, 0.0, -1000.0);
        assert_eq!(dock.size(Zone::Bottom), 440.0);
        dock.resize_zone(Zone::CanvasTop, 140.0, 0.0, -1000.0);
        assert_eq!(dock.size(Zone::CanvasTop), 80.0);
        assert_eq!(dock.current, "Editing");
        dock.resized();
        assert_eq!(dock.current, "Editing (modified)");
    }

    #[test]
    fn dividers_trade_weight_and_keep_a_minimum() {
        let mut dock = Dock::new();
        // Two groups of weight 1 in a 540px column: 2 / 500 weight per pixel.
        dock.resize_between(Zone::Left, 1, (1.0, 1.0), 50.0, 540.0);
        let weights: Vec<f64> = dock.groups(Zone::Left).iter().map(|g| g.weight).collect();
        assert!((weights[0] - 1.2).abs() < 1e-9 && (weights[1] - 0.8).abs() < 1e-9);
        dock.resize_between(Zone::Left, 1, (1.0, 1.0), -10_000.0, 540.0);
        let least = 2.0 * 70.0 / 540.0;
        assert!((dock.groups(Zone::Left)[0].weight - least).abs() < 1e-9);
    }

    #[test]
    fn saving_applying_and_resetting_layouts() {
        let mut dock = Dock::new();
        dock.close(Outputs);
        dock.save_as_new();
        assert_eq!(dock.current, "My layout 1");
        dock.apply(LIVE);
        assert_eq!(dock.current, "Live");
        assert_eq!(
            shape(&dock),
            vec![
                (Zone::Left, vec![vec![Scenes]]),
                (Zone::CanvasBottom, vec![vec![Mixer], vec![Transitions]]),
                (Zone::Right, vec![vec![Outputs]]),
            ]
        );
        assert_eq!(dock.size(Zone::Left), 260.0);
        dock.apply("My layout 1");
        assert!(!dock.shown(Outputs));
        dock.save_as_new();
        assert_eq!(dock.current, "My layout 2");
        dock.reset();
        assert_eq!(dock.current, "Editing");
        assert_eq!(shape(&dock), shape(&Dock::new()));
        assert_eq!(dock.saved.len(), 4);
    }

    #[test]
    fn the_dock_round_trips_through_storage() {
        let mut dock = Dock::new();
        let mixer = id_of(&dock, Mixer);
        dock.place(Outputs, Target::Tabs(mixer));
        dock.resize_zone(Zone::Left, 300.0, 20.0, 0.0);
        dock.save_as_new();
        let back = Dock::from_json(&dock.to_json()).expect("parses");
        assert_eq!(shape(&back), shape(&dock));
        assert_eq!(back.current, dock.current);
        assert_eq!(back.size(Zone::Left), 320.0);
        assert_eq!(back.saved, dock.saved);
        assert_eq!(back.group_of(Mixer).expect("group").active, Outputs);
    }

    #[test]
    fn stored_docks_are_checked() {
        assert_eq!(Dock::from_json("not json"), None);
        assert_eq!(Dock::from_json(r#"{"version":2}"#), None);
        let text = r#"{"version":1,"current":"Mine",
            "arrangement":{"zones":{"left":[{"tabs":["scenes","multiview"],"active":"multiview"},
                {"tabs":["scenes","mixer"],"weight":-3}],"nowhere":[{"tabs":["objects"]}]},
                "sizes":{"left":9999,"right":"wide"}},
            "saved":[{"name":"Editing","arrangement":{"zones":{},"sizes":{}}}]}"#;
        let dock = Dock::from_json(text).expect("parses");
        assert_eq!(
            shape(&dock),
            vec![(Zone::Left, vec![vec![Scenes], vec![Mixer]])]
        );
        let groups = dock.groups(Zone::Left);
        assert_eq!(groups[0].active, Scenes);
        assert_eq!(groups[1].weight, 1.0);
        assert_eq!(dock.size(Zone::Left), 560.0);
        assert_eq!(dock.size(Zone::Right), 340.0);
        assert_eq!(dock.current, "Mine");
    }
}
