//! The dock's model: which panels sit in which of the workspace's zones, grouped and
//! tabbed, how big each zone is, and the saved layouts. Every change the workspace and
//! the header's View and Layout menus make goes through [`Dock`], which is plain data so
//! it is tested natively; `workspace.rs` draws it.
//!
//! Four zones sit around the canvas (the sidebars, and the docks above and below it).
//! Each of the four outer edges holds bands: full-height columns at the left and right,
//! full-width rows at the top and bottom. An edge has as many as are in use: a panel
//! dropped beyond its outermost band starts another, and a band its last panel leaves
//! is removed, the ones beyond moving in. An edge with none keeps one empty band, which
//! a drag shows as a place to drop.
//!
//! Docs: docs/UI-FEATURES.md#docking

use serde_json::{Value, json};

/// The name of the default layout.
pub const EDITING: &str = "Editing";
/// The name of the layout for streaming.
pub const LIVE: &str = "Live";
/// The name of the layout for running a show: the Show panel along the bottom.
pub const SHOW: &str = "Show";
/// What a layout's name gets once it is changed.
const MODIFIED: &str = " (modified)";

/// One of the workspace's four outer edges, which hold bands of panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    /// Full-height columns at the left, outside the top and bottom rows.
    OuterLeft,
    /// Full-width rows at the top, between the outer columns.
    Top,
    /// Full-width rows at the bottom, between the outer columns.
    Bottom,
    /// Full-height columns at the right, outside the top and bottom rows.
    OuterRight,
}

impl Edge {
    /// Every edge.
    pub const ALL: [Edge; 4] = [Edge::OuterLeft, Edge::Top, Edge::Bottom, Edge::OuterRight];

    /// The edge's band `band`, 0 nearest the canvas.
    pub fn zone(self, band: usize) -> Zone {
        match self {
            Edge::OuterLeft => Zone::OuterLeft(band),
            Edge::Top => Zone::Top(band),
            Edge::Bottom => Zone::Bottom(band),
            Edge::OuterRight => Zone::OuterRight(band),
        }
    }

    /// Whether the workspace draws the edge's outermost band first (left and top).
    pub fn outermost_first(self) -> bool {
        matches!(self, Edge::OuterLeft | Edge::Top)
    }
}

/// One of the workspace's dock zones. An outer edge's zones carry their band, 0 nearest
/// the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Zone {
    /// A full-height column at the left, under the header.
    OuterLeft(usize),
    /// A full-width row at the top, between the outer columns.
    Top(usize),
    /// The left sidebar.
    Left,
    /// Above the canvas.
    CanvasTop,
    /// Below the canvas.
    CanvasBottom,
    /// The right sidebar.
    Right,
    /// A full-width row at the bottom, between the outer columns.
    Bottom(usize),
    /// A full-height column at the right.
    OuterRight(usize),
}

impl Zone {
    /// The zones around the canvas, which are always there.
    pub const INNER: [Zone; 4] = [Zone::Left, Zone::CanvasTop, Zone::CanvasBottom, Zone::Right];

    /// The zone's edge and band, for a zone on an outer edge.
    pub fn band(self) -> Option<(Edge, usize)> {
        match self {
            Zone::OuterLeft(band) => Some((Edge::OuterLeft, band)),
            Zone::Top(band) => Some((Edge::Top, band)),
            Zone::Bottom(band) => Some((Edge::Bottom, band)),
            Zone::OuterRight(band) => Some((Edge::OuterRight, band)),
            _ => None,
        }
    }

    /// Where the zone comes in the workspace's reading order: left to right, then top to
    /// bottom.
    fn order(self) -> (u8, isize) {
        let out = |band: usize| -band.cast_signed();
        match self {
            Zone::OuterLeft(band) => (0, out(band)),
            Zone::Top(band) => (1, out(band)),
            Zone::Left => (2, 0),
            Zone::CanvasTop => (3, 0),
            Zone::CanvasBottom => (4, 0),
            Zone::Right => (5, 0),
            Zone::Bottom(band) => (6, band.cast_signed()),
            Zone::OuterRight(band) => (7, band.cast_signed()),
        }
    }

    /// The zone's key in a stored layout: an edge's first band keeps the key it had
    /// before edges held more than one, and band `n` adds `n + 1` ("bottom2").
    pub fn key(self) -> String {
        let base = match self {
            Zone::OuterLeft(_) => "oleft",
            Zone::Top(_) => "top",
            Zone::Left => "left",
            Zone::CanvasTop => "ctop",
            Zone::CanvasBottom => "cbottom",
            Zone::Right => "right",
            Zone::Bottom(_) => "bottom",
            Zone::OuterRight(_) => "oright",
        };
        match self.band() {
            Some((_, band)) if band > 0 => format!("{base}{}", band + 1),
            _ => base.to_owned(),
        }
    }

    /// The zone a stored key names.
    pub fn from_key(key: &str) -> Option<Zone> {
        if let Some(zone) = Zone::INNER.into_iter().find(|zone| zone.key() == key) {
            return Some(zone);
        }
        Edge::ALL.into_iter().find_map(|edge| {
            let base = edge.zone(0).key();
            let rest = key.strip_prefix(base.as_str())?;
            if rest.is_empty() {
                return Some(edge.zone(0));
            }
            let n: usize = rest
                .parse()
                .ok()
                .filter(|n| *n >= 2 && !rest.starts_with('0'))?;
            Some(edge.zone(n - 1))
        })
    }

    /// Whether the zone is a column, stacking its groups top to bottom. The others place
    /// them side by side.
    pub fn vertical(self) -> bool {
        matches!(
            self,
            Zone::OuterLeft(_) | Zone::Left | Zone::Right | Zone::OuterRight(_)
        )
    }

    /// The smallest and largest size, in pixels: a column's width or a row's height.
    pub fn limits(self) -> (f64, f64) {
        match self {
            Zone::OuterLeft(_) | Zone::OuterRight(_) | Zone::Left => (180.0, 560.0),
            Zone::Right => (200.0, 560.0),
            Zone::Top(_) => (90.0, 400.0),
            // Tall enough for the Show panel's Run view and its tracks.
            Zone::Bottom(_) => (90.0, 720.0),
            Zone::CanvasTop | Zone::CanvasBottom => (80.0, 400.0),
        }
    }

    /// The size a zone starts at: outer left, top, left, above and below the canvas,
    /// right, bottom, outer right.
    pub fn default_size(self) -> f64 {
        match self {
            Zone::OuterLeft(_) => 260.0,
            Zone::Top(_) => 140.0,
            Zone::Left => 300.0,
            Zone::CanvasTop => 140.0,
            Zone::CanvasBottom => 170.0,
            Zone::Right => 340.0,
            Zone::Bottom(_) => 190.0,
            Zone::OuterRight(_) => 280.0,
        }
    }

    /// The zone's accessible name. An edge's first band has the name it always had, and
    /// the next ones count from 2 outwards ("Bottom dock 2").
    pub fn label(self) -> String {
        let base = match self {
            Zone::OuterLeft(_) => "Outer left column",
            Zone::Top(_) => "Top dock",
            Zone::Left => "Left sidebar",
            Zone::CanvasTop => "Dock above the canvas",
            Zone::CanvasBottom => "Dock below the canvas",
            Zone::Right => "Right sidebar",
            Zone::Bottom(_) => "Bottom dock",
            Zone::OuterRight(_) => "Outer right column",
        };
        match self.band() {
            Some((_, band)) if band > 0 => format!("{base} {}", band + 1),
            _ => base.to_owned(),
        }
    }

    /// The accessible name of the zone's splitter.
    pub fn splitter_label(self) -> String {
        let label = self.label();
        let mut chars = label.chars();
        let rest: String = chars
            .next()
            .map(|c| c.to_ascii_lowercase())
            .into_iter()
            .chain(chars)
            .collect();
        format!("Resize {rest}")
    }

    /// What an empty zone says while a panel is dragged. Past an edge's first band it
    /// offers a new one.
    pub fn placeholder(self) -> &'static str {
        match self {
            Zone::OuterLeft(0) => "Drop here: outer left",
            Zone::Top(0) => "Drop here: full-width top",
            Zone::Left => "Drop here: left",
            Zone::CanvasTop => "Drop here: above the canvas",
            Zone::CanvasBottom => "Drop here: below the canvas",
            Zone::Right => "Drop here: right",
            Zone::Bottom(0) => "Drop here: full-width bottom",
            Zone::OuterRight(0) => "Drop here: outer right",
            Zone::OuterLeft(_) => "Drop here: another column, outer left",
            Zone::Top(_) => "Drop here: another full-width row, top",
            Zone::Bottom(_) => "Drop here: another full-width row, bottom",
            Zone::OuterRight(_) => "Drop here: another column, outer right",
        }
    }

    /// Whether the splitter comes after the zone, which it does for zones left of or
    /// above the canvas; it always sits on the canvas side.
    pub fn splitter_after(self) -> bool {
        matches!(
            self,
            Zone::OuterLeft(_) | Zone::Top(_) | Zone::Left | Zone::CanvasTop
        )
    }

    /// How a splitter drag of `(dx, dy)` pixels changes the zone's size: it grows
    /// towards the canvas.
    pub fn grow(self, dx: f64, dy: f64) -> f64 {
        match self {
            Zone::OuterLeft(_) | Zone::Left => dx,
            Zone::Right | Zone::OuterRight(_) => -dx,
            Zone::Top(_) | Zone::CanvasTop => dy,
            Zone::CanvasBottom | Zone::Bottom(_) => -dy,
        }
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
    /// The channels and the open channel's card (ADR-0183).
    Channels,
    /// The open sequence: its lanes and triggers (ADR-0184).
    Sequence,
    /// Everything that is mapped: sources, channels and targets in one table.
    Mappings,
    /// Live control of the machine's PTZ cameras (ADR-0146, ADR-0209).
    Ptz,
    /// A show: its timeline (Edit) or its show-time controls (Run).
    Show,
}

impl PanelId {
    /// Every panel, in the View menu's order.
    pub const ALL: [PanelId; 16] = [
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
        PanelId::Channels,
        PanelId::Sequence,
        PanelId::Mappings,
        PanelId::Ptz,
        PanelId::Show,
    ];

    /// The panel's title, on its tab and in the View menu.
    pub fn title(self) -> &'static str {
        match self {
            PanelId::Scenes => "Scenes",
            PanelId::Objects => "Sources",
            PanelId::Inspector => "Inspector",
            PanelId::Mixer => "Audio mixer",
            PanelId::Outputs => "Outputs",
            PanelId::Transitions => "Transitions",
            PanelId::Transform => "Arrange",
            PanelId::Filters => "Filters",
            PanelId::Chat => "Live chat",
            PanelId::Stats => "Stream stats",
            PanelId::Agent => "Agent activity",
            PanelId::Channels => "Channels",
            PanelId::Sequence => "Sequence",
            PanelId::Mappings => "Mappings",
            PanelId::Ptz => "PTZ cameras",
            PanelId::Show => "Show",
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
            PanelId::Channels => "channels",
            PanelId::Sequence => "sequence",
            PanelId::Mappings => "mappings",
            PanelId::Ptz => "ptz",
            PanelId::Show => "show",
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
            | PanelId::Agent
            | PanelId::Channels
            | PanelId::Ptz => Zone::Right,
            PanelId::Mixer
            | PanelId::Outputs
            | PanelId::Transitions
            | PanelId::Sequence
            | PanelId::Mappings
            | PanelId::Show => Zone::Bottom(0),
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
    /// As a new group at the end of a zone. An edge's band past its last one is a new
    /// band.
    End(Zone),
}

/// One zone's groups and size.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneState {
    /// The zone.
    pub zone: Zone,
    /// Its groups, in order.
    pub groups: Vec<Group>,
    /// Its size in pixels: a column's width or a row's height.
    pub size: f64,
}

/// Which panels sit where, and how big each zone is.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrangement {
    /// The zones, in the workspace's reading order ([`Zone::order`]): the four inner
    /// ones always, and each edge's bands, at least one.
    pub zones: Vec<ZoneState>,
}

impl Arrangement {
    /// An arrangement of empty zones at their default sizes: the inner ones and one band
    /// on each edge.
    fn empty() -> Arrangement {
        let mut zones: Vec<ZoneState> = Zone::INNER
            .into_iter()
            .chain(Edge::ALL.into_iter().map(|edge| edge.zone(0)))
            .map(|zone| ZoneState {
                zone,
                groups: Vec::new(),
                size: zone.default_size(),
            })
            .collect();
        zones.sort_by_key(|z| z.zone.order());
        Arrangement { zones }
    }

    /// `zone`'s state, if it is there.
    fn state(&self, zone: Zone) -> Option<&ZoneState> {
        self.zones.iter().find(|z| z.zone == zone)
    }

    /// `zone`'s state, made (empty, at its default size) if it isn't there. A band past
    /// its edge's last one becomes the next band.
    fn state_mut(&mut self, zone: Zone) -> &mut ZoneState {
        let zone = match zone.band() {
            Some((edge, band)) => edge.zone(band.min(self.bands(edge))),
            None => zone,
        };
        let at = match self.zones.iter().position(|z| z.zone == zone) {
            Some(at) => at,
            None => {
                self.zones.push(ZoneState {
                    zone,
                    groups: Vec::new(),
                    size: zone.default_size(),
                });
                self.zones.sort_by_key(|z| z.zone.order());
                self.zones
                    .iter()
                    .position(|z| z.zone == zone)
                    .unwrap_or_default()
            }
        };
        &mut self.zones[at]
    }

    /// How many bands `edge` has.
    fn bands(&self, edge: Edge) -> usize {
        self.zones
            .iter()
            .filter(|z| z.zone.band().is_some_and(|(e, _)| e == edge))
            .count()
    }

    /// Drops each edge's empty bands, the ones beyond moving in, and keeps one band on an
    /// edge left with none (an empty first band keeps its size).
    fn tidy(&mut self) {
        for edge in Edge::ALL {
            let bands: Vec<ZoneState> = self
                .zones
                .iter()
                .filter(|z| z.zone.band().is_some_and(|(e, _)| e == edge))
                .cloned()
                .collect();
            let mut kept: Vec<ZoneState> = bands
                .iter()
                .filter(|z| !z.groups.is_empty())
                .cloned()
                .collect();
            if kept.is_empty() {
                let zone = edge.zone(0);
                let size = bands
                    .iter()
                    .find(|z| z.zone == zone)
                    .map_or(zone.default_size(), |z| z.size);
                kept.push(ZoneState {
                    zone,
                    groups: Vec::new(),
                    size,
                });
            }
            kept.sort_by_key(|z| z.zone.band().map_or(0, |(_, band)| band));
            for (band, state) in kept.iter_mut().enumerate() {
                state.zone = edge.zone(band);
            }
            self.zones
                .retain(|z| !z.zone.band().is_some_and(|(e, _)| e == edge));
            self.zones.extend(kept);
        }
        self.zones.sort_by_key(|z| z.zone.order());
    }
}

/// A saved layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Its name in the Layout menu.
    pub name: String,
    /// What it holds. Group IDs are renumbered when it is applied.
    pub arrangement: Arrangement,
}

/// The default layout: Scenes over Sources on the left, the inspector with the Filters
/// and Arrange panels as tabs of one full-height group on the right, and the mixer and
/// the outputs (with Transitions as a second tab) along the bottom.
pub fn editing() -> Arrangement {
    use PanelId::*;
    arrangement(
        &[],
        &[
            (Zone::Left, vec![(vec![Scenes], 1.0), (vec![Objects], 1.0)]),
            (
                Zone::Right,
                vec![(vec![Inspector, Filters, Transform], 1.0)],
            ),
            (
                Zone::Bottom(0),
                vec![(vec![Mixer], 1.0), (vec![Outputs, Transitions], 1.0)],
            ),
        ],
    )
}

/// The layout for streaming: Scenes on the left, the mixer and Transitions under the
/// canvas, and Stream stats with the outputs and Agent activity as tabs on the right.
pub fn live() -> Arrangement {
    use PanelId::*;
    arrangement(
        &[(Zone::Left, 260.0), (Zone::CanvasBottom, 180.0)],
        &[
            (Zone::Left, vec![(vec![Scenes], 1.0)]),
            (
                Zone::CanvasBottom,
                vec![(vec![Mixer], 1.0), (vec![Transitions], 1.0)],
            ),
            (Zone::Right, vec![(vec![Stats, Outputs, Agent], 1.0)]),
        ],
    )
}

/// The layout for running a show: Scenes on the left, the Inspector on the right, and
/// the Show panel alone along the bottom, tall enough for its Run view and its tracks.
pub fn show() -> Arrangement {
    use PanelId::*;
    arrangement(
        &[(Zone::Left, 260.0), (Zone::Bottom(0), 560.0)],
        &[
            (Zone::Left, vec![(vec![Scenes], 1.0)]),
            (Zone::Right, vec![(vec![Inspector], 1.0)]),
            (Zone::Bottom(0), vec![(vec![Show], 1.0)]),
        ],
    )
}

/// A zone's groups in a built-in layout: each group's tabs, the first active, and its
/// weight.
type Groups = Vec<(Vec<PanelId>, f64)>;

/// An arrangement of `groups` per zone, with `sizes` replacing the defaults. IDs are
/// placeholders until [`Dock::adopt`] numbers them.
fn arrangement(sizes: &[(Zone, f64)], groups: &[(Zone, Groups)]) -> Arrangement {
    let mut arranged = Arrangement::empty();
    for &(zone, size) in sizes {
        arranged.state_mut(zone).size = size;
    }
    for (zone, list) in groups {
        arranged.state_mut(*zone).groups = list
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
    arranged.tidy();
    arranged
}

/// The dock: the arrangement on screen, its layout's name and the saved layouts.
#[derive(Clone, Debug, PartialEq)]
pub struct Dock {
    /// What is on screen.
    pub arrangement: Arrangement,
    /// The layout's name, with " (modified)" once it was changed.
    pub current: String,
    /// The saved layouts, Editing, Live and Show first.
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
            arrangement: Arrangement::empty(),
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
                Layout {
                    name: SHOW.into(),
                    arrangement: show(),
                },
            ],
            next_id: 100,
        };
        dock.adopt(editing());
        dock
    }

    /// Puts `arrangement` on screen with fresh group IDs.
    fn adopt(&mut self, mut arrangement: Arrangement) {
        for group in arrangement.zones.iter_mut().flat_map(|z| &mut z.groups) {
            group.id = self.next_id;
            self.next_id += 1;
        }
        self.arrangement = arrangement;
    }

    /// Every zone there is, in the workspace's reading order.
    pub fn zones(&self) -> impl Iterator<Item = Zone> + '_ {
        self.arrangement.zones.iter().map(|z| z.zone)
    }

    /// How many bands `edge` has: at least one.
    pub fn bands(&self, edge: Edge) -> usize {
        self.arrangement.bands(edge)
    }

    /// The groups in `zone`: none for a band that isn't there.
    pub fn groups(&self, zone: Zone) -> &[Group] {
        self.arrangement
            .state(zone)
            .map_or(&[], |z| z.groups.as_slice())
    }

    /// `zone`'s size in pixels: its default for a band that isn't there.
    pub fn size(&self, zone: Zone) -> f64 {
        self.arrangement
            .state(zone)
            .map_or(zone.default_size(), |z| z.size)
    }

    /// Every group, zone by zone.
    fn all_groups(&self) -> impl Iterator<Item = &Group> {
        self.arrangement.zones.iter().flat_map(|z| &z.groups)
    }

    /// Every group, zone by zone, to change.
    fn all_groups_mut(&mut self) -> impl Iterator<Item = &mut Group> {
        self.arrangement
            .zones
            .iter_mut()
            .flat_map(|z| &mut z.groups)
    }

    /// Group `id`, if it exists.
    pub fn group(&self, id: u32) -> Option<&Group> {
        self.all_groups().find(|g| g.id == id)
    }

    /// Where group `id` is.
    pub fn place_of(&self, id: u32) -> Option<Place> {
        self.arrangement.zones.iter().find_map(|z| {
            z.groups.iter().position(|g| g.id == id).map(|index| Place {
                zone: z.zone,
                index,
            })
        })
    }

    /// The group showing `panel`, if it is docked.
    pub fn group_of(&self, panel: PanelId) -> Option<&Group> {
        self.all_groups().find(|g| g.tabs.contains(&panel))
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

    /// Takes `panel` out of its group, deleting the group if it empties. The bands stay
    /// as they are until [`Arrangement::tidy`], so a target named before still points
    /// where it did.
    fn take(&mut self, panel: PanelId) {
        for state in &mut self.arrangement.zones {
            for group in &mut state.groups {
                if let Some(at) = group.tabs.iter().position(|&p| p == panel) {
                    group.tabs.remove(at);
                    if group.active == panel
                        && let Some(&next) = group.tabs.get(at.min(group.tabs.len().max(1) - 1))
                    {
                        group.active = next;
                    }
                }
            }
            state.groups.retain(|g| !g.tabs.is_empty());
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
    /// group, or on the tabs it is already in, changes nothing. A target past an edge's
    /// last band starts a new band there. Returns whether the layout changed.
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
                let group = &mut self.arrangement.state_mut(place.zone).groups[place.index];
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
                self.arrangement
                    .state_mut(place.zone)
                    .groups
                    .insert(at, group);
            }
            Target::End(zone) => {
                let group = self.lone(panel);
                self.arrangement.state_mut(zone).groups.push(group);
            }
        }
        self.arrangement.tidy();
        self.modified();
        true
    }

    /// Closes `panel`.
    pub fn close(&mut self, panel: PanelId) {
        if self.shown(panel) {
            self.take(panel);
            self.arrangement.tidy();
            self.modified();
        }
    }

    /// The View menu's toggle: closes a shown panel, or adds a hidden one in its home
    /// zone.
    pub fn toggle(&mut self, panel: PanelId) {
        if self.shown(panel) {
            self.take(panel);
            self.arrangement.tidy();
        } else {
            let group = self.lone(panel);
            self.arrangement.state_mut(panel.home()).groups.push(group);
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
            self.arrangement.state_mut(panel.home()).groups.push(group);
        }
    }

    /// Makes `panel` its group's shown tab and expands the group.
    fn activate(&mut self, panel: PanelId) {
        if let Some(group) = self.all_groups_mut().find(|g| g.tabs.contains(&panel)) {
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
        let group = &mut self.arrangement.state_mut(place.zone).groups[place.index];
        if collapsible && group.active == panel {
            group.collapsed = !group.collapsed;
        } else if group.tabs.contains(&panel) {
            group.active = panel;
            group.collapsed = false;
        }
    }

    /// Sets `zone`'s size to `from` plus a splitter drag of `(dx, dy)`, within the zone's
    /// limits. A band that isn't there has no splitter, so nothing changes.
    pub fn resize_zone(&mut self, zone: Zone, from: f64, dx: f64, dy: f64) {
        if self.arrangement.state(zone).is_none() {
            return;
        }
        let (min, max) = zone.limits();
        self.arrangement.state_mut(zone).size = (from + zone.grow(dx, dy)).clamp(min, max);
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
        if self.arrangement.state(zone).is_none() {
            return;
        }
        let groups = &mut self.arrangement.state_mut(zone).groups;
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

    /// Saves what is on screen as "My layout N", the first N no saved layout has, and
    /// makes it current.
    pub fn save_as_new(&mut self) {
        let name = (1..)
            .map(|n| format!("My layout {n}"))
            .find(|name| !self.saved.iter().any(|l| &l.name == name))
            .unwrap_or_default();
        let mut arrangement = self.arrangement.clone();
        for group in arrangement.zones.iter_mut().flat_map(|z| &mut z.groups) {
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

    /// The dock as JSON text, for the browser's storage.
    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }

    /// The dock as a JSON value, for the browser's storage and the engine's UI settings
    /// (ADR-0084).
    pub fn to_value(&self) -> Value {
        json!({
            "version": 1,
            "current": self.current,
            "arrangement": arrangement_json(&self.arrangement),
            "saved": self.saved.iter().map(|l| json!({
                "name": l.name,
                "arrangement": arrangement_json(&l.arrangement),
            })).collect::<Vec<_>>(),
        })
    }

    /// The dock [`Dock::to_json`] stored, if `text` is one. Panels this UI doesn't know
    /// are dropped, a panel docked twice keeps its first place, and sizes are brought
    /// within limits.
    pub fn from_json(text: &str) -> Option<Dock> {
        Dock::from_value(&serde_json::from_str(text).ok()?)
    }

    /// The dock [`Dock::to_value`] made, if `value` is one, read as
    /// [`Dock::from_json`] reads it.
    pub fn from_value(value: &Value) -> Option<Dock> {
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
        // A built-in layout newer than the stored dock joins its saved ones.
        if !saved.iter().any(|l| l.name == SHOW) {
            saved.push(Layout {
                name: SHOW.into(),
                arrangement: show(),
            });
        }
        dock.saved = saved;
        dock.adopt(arrangement);
        dock.current = value["current"].as_str()?.to_owned();
        Some(dock)
    }
}

/// An arrangement as JSON: each zone's groups and size under its key. A UI from before
/// edges held more than one band reads the first band of each and skips the rest.
fn arrangement_json(arrangement: &Arrangement) -> Value {
    let mut zones = serde_json::Map::new();
    let mut sizes = serde_json::Map::new();
    for state in &arrangement.zones {
        zones.insert(
            state.zone.key(),
            state
                .groups
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
        sizes.insert(state.zone.key(), json!(state.size));
    }
    json!({ "zones": zones, "sizes": sizes })
}

/// The arrangement `value` holds, if it is one. An edge's bands are read in order and
/// renumbered, so a missing or empty one leaves no gap.
fn parse_arrangement(value: &Value) -> Option<Arrangement> {
    let mut parsed = Arrangement::empty();
    let mut seen = Vec::new();
    let mut zones: Vec<(Zone, &Value)> = value["zones"]
        .as_object()?
        .iter()
        .filter_map(|(key, groups)| Zone::from_key(key).map(|zone| (zone, groups)))
        .collect();
    // Bands in order, so each lands as the next one (`state_mut`).
    zones.sort_by_key(|(zone, _)| (zone.order().0, zone.band().map_or(0, |(_, band)| band)));
    let mut placed: Vec<(Zone, Zone)> = Vec::new();
    for (zone, groups) in zones {
        let mut list = Vec::new();
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
            list.push(Group {
                id: 0,
                tabs,
                active,
                weight,
                collapsed: group["collapsed"].as_bool().unwrap_or(false),
            });
        }
        // An empty band past the first takes no place, so the next one moves in; a
        // band past a gap becomes the next one.
        let into = match zone.band() {
            Some((_, band)) if band > 0 && list.is_empty() => continue,
            Some((edge, band)) => edge.zone(band.min(parsed.bands(edge))),
            None => zone,
        };
        parsed.state_mut(into).groups = list;
        placed.push((zone, into));
    }
    let always = Zone::INNER
        .into_iter()
        .chain(Edge::ALL.into_iter().map(|edge| edge.zone(0)))
        .map(|zone| (zone, zone));
    for (stored, zone) in placed.into_iter().chain(always) {
        if let Some(size) = value["sizes"][stored.key()].as_f64() {
            let (min, max) = zone.limits();
            if size.is_finite() {
                parsed.state_mut(zone).size = size.clamp(min, max);
            }
        }
    }
    parsed.tidy();
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panels in each non-empty zone, by group.
    fn shape(dock: &Dock) -> Vec<(Zone, Vec<Vec<PanelId>>)> {
        dock.zones()
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
                (Zone::Right, vec![vec![Inspector, Filters, Transform]]),
                (
                    Zone::Bottom(0),
                    vec![vec![Mixer], vec![Outputs, Transitions]]
                ),
            ]
        );
        assert_eq!(dock.size(Zone::Left), 300.0);
        assert_eq!(dock.size(Zone::Right), 340.0);
        assert_eq!(dock.size(Zone::Bottom(0)), 190.0);
        let names: Vec<_> = dock.saved.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Editing", "Live", "Show"]);
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
        assert_eq!(dock.groups(Zone::Bottom(0)).len(), 2);
        assert_eq!(dock.groups(Zone::Bottom(0))[1].tabs, [Transitions]);
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
                        vec![Inspector, Filters, Transform],
                        vec![Mixer]
                    ]
                ),
                (Zone::Bottom(0), vec![vec![Outputs, Transitions]]),
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
        assert!(dock.place(Inspector, Target::End(Zone::OuterRight(0))));
        assert_eq!(dock.groups(Zone::Right).len(), 1);
        assert_eq!(dock.groups(Zone::OuterRight(0))[0].tabs, [Inspector]);
        assert!(dock.place(Transform, Target::End(Zone::OuterRight(0))));
        assert_eq!(dock.groups(Zone::Right)[0].tabs, [Filters]);
        assert!(dock.place(Filters, Target::End(Zone::OuterRight(0))));
        assert!(dock.groups(Zone::Right).is_empty());
    }

    #[test]
    fn leaving_a_tab_group_activates_a_neighbour() {
        let mut dock = Dock::new();
        let mixer = id_of(&dock, Mixer);
        dock.place(Outputs, Target::Tabs(mixer));
        dock.place(Scenes, Target::Tabs(mixer));
        dock.place(Scenes, Target::End(Zone::Top(0)));
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
            dock.groups(Zone::Bottom(0)).last().expect("group").tabs,
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
        dock.resize_zone(Zone::Bottom(0), 190.0, 0.0, -1000.0);
        assert_eq!(dock.size(Zone::Bottom(0)), 720.0);
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
    fn a_dock_stored_before_the_show_layout_gains_it() {
        let mut value = Dock::new().to_value();
        if let Some(saved) = value["saved"].as_array_mut() {
            saved.retain(|l| l["name"] != SHOW);
        }
        let dock = Dock::from_value(&value).expect("a dock");
        let names: Vec<_> = dock.saved.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Editing", "Live", "Show"]);
        let mut dock = dock;
        dock.apply(SHOW);
        assert_eq!(dock.groups(Zone::Bottom(0))[0].tabs, [Show]);
        assert_eq!(dock.size(Zone::Bottom(0)), 560.0);
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
                (Zone::Right, vec![vec![Stats, Outputs, Agent]]),
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
        assert_eq!(dock.saved.len(), 5);
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

    #[test]
    fn a_drop_past_an_edge_adds_a_band_and_an_emptied_band_goes() {
        let mut dock = Dock::new();
        assert_eq!(dock.bands(Edge::Bottom), 1);
        // Past the bottom's only band: a second full-width row.
        assert!(dock.place(Sequence, Target::End(Zone::Bottom(1))));
        assert_eq!(dock.bands(Edge::Bottom), 2);
        assert_eq!(dock.groups(Zone::Bottom(1))[0].tabs, [Sequence]);
        // Any band number past the last is the next one.
        assert!(dock.place(Chat, Target::End(Zone::Bottom(7))));
        assert_eq!(dock.groups(Zone::Bottom(2))[0].tabs, [Chat]);
        dock.resize_zone(Zone::Bottom(2), 190.0, 0.0, -30.0);
        assert_eq!(dock.size(Zone::Bottom(2)), 220.0);
        // Emptying the middle band moves the outer one in, with its size.
        dock.close(Sequence);
        assert_eq!(dock.bands(Edge::Bottom), 2);
        assert_eq!(dock.groups(Zone::Bottom(1))[0].tabs, [Chat]);
        assert_eq!(dock.size(Zone::Bottom(1)), 220.0);
        // The same holds on every edge: columns at the sides, rows at the top.
        for edge in [Edge::OuterLeft, Edge::Top, Edge::OuterRight] {
            assert!(dock.place(Agent, Target::End(edge.zone(0))));
            assert!(dock.place(Stats, Target::End(edge.zone(1))));
            assert_eq!(dock.bands(edge), 2);
            dock.close(Agent);
            assert_eq!(dock.bands(edge), 1);
            assert_eq!(dock.groups(edge.zone(0))[0].tabs, [Stats]);
            dock.close(Stats);
            // An edge left with nothing keeps one empty band.
            assert_eq!(dock.bands(edge), 1);
            assert!(dock.groups(edge.zone(0)).is_empty());
        }
    }

    #[test]
    fn a_drop_into_a_band_its_panel_empties_still_lands_there() {
        let mut dock = Dock::new();
        dock.place(Sequence, Target::End(Zone::Bottom(1)));
        dock.place(Chat, Target::End(Zone::Bottom(2)));
        // Moving the second band's only panel onto the third: two bands, Chat's first
        // in the second.
        assert!(dock.place(Sequence, Target::End(Zone::Bottom(2))));
        assert_eq!(dock.bands(Edge::Bottom), 2);
        assert_eq!(
            dock.groups(Zone::Bottom(1))
                .iter()
                .map(|g| g.tabs.clone())
                .collect::<Vec<_>>(),
            [vec![Chat], vec![Sequence]]
        );
    }

    #[test]
    fn bands_round_trip_and_older_docks_still_load() {
        let mut dock = Dock::new();
        dock.place(Sequence, Target::End(Zone::Bottom(1)));
        dock.place(Chat, Target::End(Zone::OuterLeft(0)));
        dock.place(Stats, Target::End(Zone::OuterLeft(1)));
        dock.resize_zone(Zone::OuterLeft(1), 260.0, 40.0, 0.0);
        let value = dock.to_value();
        assert_eq!(
            value["arrangement"]["zones"]["bottom2"][0]["tabs"][0],
            "sequence"
        );
        assert_eq!(value["arrangement"]["sizes"]["oleft2"], 300.0);
        let back = Dock::from_value(&value).expect("parses");
        assert_eq!(shape(&back), shape(&dock));
        assert_eq!(back.size(Zone::OuterLeft(1)), 300.0);
        // A gap in the stored bands closes up, and an unknown band number is skipped.
        let text = r#"{"version":1,"current":"Mine",
            "arrangement":{"zones":{"bottom":[],"bottom3":[{"tabs":["sequence"]}],
                "bottom02":[{"tabs":["chat"]}]},"sizes":{"bottom3":250}},
            "saved":[{"name":"Editing","arrangement":{"zones":{},"sizes":{}}}]}"#;
        let dock = Dock::from_json(text).expect("parses");
        assert_eq!(shape(&dock), vec![(Zone::Bottom(0), vec![vec![Sequence]])]);
        assert_eq!(dock.size(Zone::Bottom(0)), 250.0);
        assert_eq!(dock.bands(Edge::Bottom), 1);
    }

    #[test]
    fn band_keys_and_labels_count_from_two() {
        assert_eq!(Zone::Bottom(0).key(), "bottom");
        assert_eq!(Zone::Bottom(1).key(), "bottom2");
        assert_eq!(Zone::from_key("oright3"), Some(Zone::OuterRight(2)));
        assert_eq!(Zone::from_key("top1"), None);
        assert_eq!(Zone::from_key("ctop"), Some(Zone::CanvasTop));
        assert_eq!(Zone::Bottom(1).label(), "Bottom dock 2");
        assert_eq!(Zone::Bottom(0).splitter_label(), "Resize bottom dock");
        assert_eq!(
            Zone::OuterLeft(2).splitter_label(),
            "Resize outer left column 3"
        );
    }
}
