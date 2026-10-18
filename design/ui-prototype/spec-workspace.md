<!-- @doc
id = "DESIGN-UI-SPEC-WORKSPACE"
title = "UI prototype spec: workspace"
kind = "reference"
status = "active"
review_every = "90d"
-->

# UI prototype spec: workspace

What [`Docking.dc.html`](Docking.dc.html), the workspace of the [UI prototype](README.md) (direction D), does, written as requirements: the theme, the header, the dock zones, the canvas, the menus, the dialogs, the keyboard and the transition engine. Panel contents are in [the panels spec](spec-panels.md). [UI features](../../docs/UI-FEATURES.md) tracks which of these the real UI has.

Line references (`L123`) point into `Docking.dc.html` as committed. *Simulated* marks prototype behaviour that is faked; it is not a requirement. "Accent" is the component's `accent` prop: `#4C8DFF` by default (L896). The logical canvas is 1920 × 1080.

<a id="theme-fonts-and-global-styling"></a>
## 0. Theme, fonts and global styling
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 0.1 | Fonts | IBM Plex Sans (400/500/600) for the UI and IBM Plex Mono (400/500) for numbers and readouts, loaded from Google Fonts. Root font is `'IBM Plex Sans', system-ui, sans-serif`, 13px. | L11, L43 |
| 0.2 | Colour scheme | `color-scheme: dark`. Body bg `#0F1115`. Root text `#E6E8EB`. Root container is `height: 100vh; min-height: 640px`, a flex column, `overflow: hidden`. | L13-14, L43 |
| 0.3 | Links | `a` is `#8AB4FF`, hover `#B5CEFF`. | L15 |
| 0.4 | Scrollbars | Thin. Thumb `#3A3F48` with radius 6px and a 2px `#16191E` border. Thumb hover `#4D535E`. Track and corner transparent. Size 10px. | L18-23 |
| 0.5 | Selection | `::selection` bg `#2B4A7A`. | L24 |
| 0.6 | Focus ring | `input/button:focus-visible` outline is 2px `#4C8DFF` at offset -1px. | L25 |
| 0.7 | Menu-item highlight | `.menuitem:hover`, `:focus-visible` and `.on` (open submenu) all use bg `#2E3D55` and text `#FFFFFF`. Disabled `.menuitem.dis` gets no highlight and `cursor: default`. | L26-29 |
| 0.8 | Tab hover | `.tab:hover` colour `#E6E8EB`. | L30 |
| 0.9 | Splitter and divider hover | bg `#2B4A7A`. | L31 |
| 0.10 | Close-button reveal | `.tclose` (per-tab ×) has opacity 0 and fades in over 120ms when its `.tabwrap` is hovered or focused. `.pclose` (lone-panel ×) has opacity 0 and appears on `.strip:hover`. Hover bg `#2B313A`. In the `.armed` state it shows at opacity 1 with font-weight 700. | L32-38 |
| 0.11 | Canvas item hover | `.cv-item:hover > .cv-hover` outline colour `rgba(138,180,255,0.6)`. The base hover outline is 1px and transparent. | L39, L330 |
| 0.12 | Canvas focus | `[data-canvas]:focus` outline colour `#3A3F48`. | L40 |
| 0.13 | Palette in use | Surfaces: `#0A0B0E` (canvas area and pop-out body), `#0F1115` (app, inputs, splitters), `#121418` (dock zone bg), `#141A22` (canvas frame), `#16191E` (header, panel groups, dialog right column), `#1A1D23` (dialogs, floating toolbar, stepper buttons), `#1F232A` (menus, secondary buttons, toast, pop-out title bar, filter rows). Borders: `#262A31` (panel and header lines), `#2E333B` (menus, dialogs, inputs), `#3A3F48` (dashed drop placeholders, toast, pop-out border, toggle-off track). Text: `#E6E8EB` primary, `#9AA1AC` secondary, `#7D8590` tertiary, `#5A606B` faint, `#C9CED6` section heads. Accent blues: `#4C8DFF`, `#8AB4FF` (checks and links), `#B5CEFF` (active tool icon), `#22324A` (primary button, selected list row, active tool), `#2B313A` (open-menu button, selected segment), `#2E3D55` (menu hover). Danger: `#C62F36` (Go Live), `#7A1E23` (live or privacy on), `#E5484D` (privacy border), `#FF5C63` (armed ×, YouTube icon), `#FF8A8F` (Remove). Crop orange: `#E8A33D`, `#4A3510` (crop tool bg), `#F5C77E` (crop tool icon). Guides magenta `#FF4FD8`. Keyframe purple: `rgba(176,124,255,0.16)` bg, `#6E4FB0` border, `#D9C6FF` text. | throughout |
| 0.14 | Common shadows | Menus `0 12px 32px rgba(0,0,0,0.5)` (context menus 0.55). Modal dialogs `0 24px 60px rgba(0,0,0,0.6)`. Pop-outs `0 18px 48px rgba(0,0,0,0.6)`. Floating toolbar `0 6px 18px rgba(0,0,0,0.45)`. | various |
| 0.15 | Modal backdrop | `position: fixed; inset: 0; background: rgba(5,6,8,0.6)`, content centred, 16px padding. A press on the backdrop closes (or cancels) the dialog. A press inside stops propagation. | L522, L592, L689, L746, L806 |
| 0.16 | Shared form controls | Text input height 32 (30 in compact dialogs), bg `#0F1115`, 1px `#2E333B` border, radius 5, padding 0 9px. Numeric stepper is 200px wide and 30px high: a `−` button (26px, bg `#1A1D23`, right border), a right-aligned mono 12px input, a unit label (11px `#7D8590`), then a `+` button. The switch has a 30x16 track (radius 8; `#4C8DFF` on, `#3A3F48` off) and a 12px white knob at left 16 (on) or 2 (off). Colour swatches are 22px circles with a 2px ring, `#E6E8EB` when selected and transparent otherwise. Primary button: bg `#22324A`, 1px `#4C8DFF` border, radius 6, padding 7px 18px, `data-primary`. Secondary button: no bg, 1px `#2E333B` border, radius 6. Disabled primary has opacity 0.45. | L528-565, L604-649, L703-731 |

---

<a id="header-bar"></a>
## 1. Header bar (left to right)
<!-- @section
-->

The header is `position: relative; z-index: 30`, a wrapping flex row (`flex-wrap: wrap`) with gap 10, padding `8px 14px`, bg `#16191E` and bottom border 1px `#262A31` (L45).

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 1.1 | Brand | A 22x22 square with radius 6 filled with accent, then **"JStream"** at 15px weight 600. Gap 10, margin-right 6. | L46-49 |
| 1.2 | File menu button | Text "File", colour `#9AA1AC`, padding 6px 10px, radius 5, class menuitem. **Simulated/no-op: it has no handler and no menu.** | L50 |
| 1.3 | Edit menu button | "Edit", styled like File. **No-op.** | L51 |
| 1.4 | View menu | Button "View" (colour `#E6E8EB`; bg `#2B313A` while open, otherwise transparent; `aria-expanded`). It opens a 240px dropdown at top 36px, left 0. Dropdown: bg `#1F232A`, 1px `#2E333B` border, radius 8, padding 6. Section head "PANELS" (11px, `#9AA1AC`, letter-spacing 0.8px). One row per panel, in this order: **Scenes, Scene objects, Inspector, Transform, Filters, Live chat, Stream stats, Audio mixer, Outputs, Agent activity, Transitions**. Each row shows the title on the left and a mark on the right: **"shown"** (`#7D8590`) when docked anywhere, **"add"** (`#8AB4FF`) when not. Clicking toggles the panel. A shown panel is removed from wherever it is. A hidden one is appended as a new group in its HOME zone. The current layout name gets " (modified)". The menu stays open after a toggle. | L52-62, L3474-3490, L897-901 |
| 1.5 | Layout menu | Button text "Layout: " + `{currentName}` (in `#9AA1AC`) + "▾" (`#7D8590`, 10px). Bg `#2B313A` while open. Dropdown is 260px. Head "SAVED LAYOUTS". Rows: each saved layout name, with "current" in `#8AB4FF` when `current === name` exactly (so nothing is marked once the name carries " (modified)"). Then a 1px `#2E333B` separator, **"Save current layout as new"** (`#8AB4FF`) and **"Reset to default"** (`#9AA1AC`). Picking a layout applies its zones and sizes, sets current and closes the menu. Save appends `"My layout " + (layouts.length - 1)` (the first is "My layout 1"), makes it current and closes. Reset restores the EDITING layout, sets current "Editing" and closes. | L63-76, L3491-3500 |
| 1.6 | Spacer | `flex: 1 1 auto`. | L77 |
| 1.7 | Stats readout | Mono 12px `#9AA1AC`, gap 14: **"60 fps"**, **"0 dropped"**, **"CPU 3%"**. **Simulated: static text.** | L78-80 |
| 1.8 | Privacy button | An eye-slash icon (14px) followed by a label: **"Privacy"** when off, **"Privacy on"** when on. Off: bg `#1F232A`, border `#2E333B`. On: bg `#7A1E23`, border `#E5484D`. Radius 6, padding 8px 12px, `aria-pressed`. Tooltip: "Privacy screen: blur the whole output and mute the mic (Pause)". It toggles `privacy`. See 4.20 and 7.1. | L81, L3436-3440 |
| 1.9 | Studio mode button | "Studio mode": bg `#1F232A`, border `#2E333B`, radius 6, padding 8px 14px. **No-op.** | L82 |
| 1.10 | Record button | "Record", same style. **No-op.** | L83 |
| 1.11 | Broadcast picker | A button 34px high and at most 280px wide (bg `#1F232A`, or `#2B313A` while open; border `#2E333B`; radius 6; padding 0 10). Contents: a 16px YouTube-style play icon stroked in `#FF5C63`; then two lines, the title (12px, ellipsised at 200px) and a sub-line (10px `#9AA1AC`); then "▾". Title is the current broadcast's title, or "No broadcast selected". Sub is `privacy · when`, or "Choose or create one". Tooltip: "Choose which YouTube broadcast Go Live uses". `aria-haspopup=menu`. | L84-92, L1849-1867 |
| 1.12 | Broadcast menu | 320px wide, anchored right 0 at top 40, role=menu. Head "YOUR BROADCASTS ON YOUTUBE". Each broadcast is a `menuitemradio`: a 14px check column (✓ in `#8AB4FF` on the selected one), the title (ellipsised) and a sub-line (11px `#9AA1AC`) reading `privacy · when · latency latency`. Picking one selects it and closes the menu. After a separator: **"+ Create new stream…"** (`#8AB4FF`), which opens the Create dialog (6.1), and **"Open in YouTube Studio"**, which is permanently disabled (opacity 0.5). Demo broadcasts: (b1) "Building JStream: the canvas editor", Unlisted, Today 19:00, Low (selected by default); (b2) "Rust + wgpu Q&A", Public, Thu 18:30, Normal; (b3) "Friday playtest", Private, Fri 20:00, Ultra-low. | L93-110, L1174-1178, L1868-1876 |
| 1.13 | Go Live button | White text, weight 600, padding 8px 18px, radius 6, no border. Label **"Go Live"** on bg `#C62F36`, or **"End stream"** on bg `#7A1E23` while live. Clicking toggles `live`. **Simulated: it only flips a flag.** `streamInfo` passes `{...broadcast, live}` to Panel. | L111, L1877-1879, L3460 |
| 1.14 | Top-menu exclusivity and closing | `state.menu` is one of `'view' \| 'layouts' \| 'stream' \| null`, so only one top menu is open at a time. Clicking its button toggles it. A capture-phase `pointerdown` anywhere outside `[data-topmenu]` closes it, even over panels or the canvas that stop propagation. The root `closeMenus` also closes the context menu on any press and skips top-menu buttons so that a second click does not re-open the menu. | L1563-1569, L3422-3427, L43 |

---

<a id="dock-zones-and-docking"></a>
## 2. Dock zones and docking
<!-- @section
-->

### 2.1 Zone geometry and DOM order

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 2.1.1 | Zones | 8 zones: `oleft` (outer left column), `top` (full-width top), `left` (left sidebar), `ctop` (above the canvas), `cbottom` (below the canvas), `right` (right sidebar), `bottom` (full-width bottom), `oright` (outer right column). | L902 |
| 2.1.2 | Nesting | The row holds **oleft**, then a middle column, then **oright**. The middle column holds **top**, then a row, then **bottom**. That row holds **left**, then the centre column, then **right**. The centre column holds **ctop**, then the **Canvas**, then **cbottom**. So top and bottom span between the outer columns, and ctop and cbottom only span the canvas column. | L114-519 |
| 2.1.3 | Vertical zones | `left`, `right`, `oleft` and `oright` stack their groups vertically: rows, with dividers that drag on Y. The others place groups side by side, with dividers that drag on X. Panel gets `vertical="yes"` in vertical zones and `"no"` otherwise. | L903, L139/173/208/243/410/445/479/513 |
| 2.1.4 | Size limits (px) | oleft 180-560; oright 180-560; left 180-560; right 200-560; top 90-400; bottom 90-440; ctop 80-400; cbottom 80-400. | L904 |
| 2.1.5 | Zone styling | `section` with aria-label "Outer left column", "Top dock", "Left sidebar", "Dock above the canvas", "Dock below the canvas", "Right sidebar", "Bottom dock" or "Outer right column". Bg `#121418`, padding 6, box-sizing border-box. Width (vertical zones) or height (horizontal zones) equals `sizes[zone]`, or **44px when the zone is empty**. | L117, L151, L186, L221, L388, L423, L457, L491, L3390 |
| 2.1.6 | Zone splitters | A 5px bar, bg `#0F1115`, hover `#2B4A7A`. Cursor `col-resize` for vertical zones and `row-resize` for horizontal ones. aria-label "Resize outer left column", "Resize top dock", "Resize left sidebar", "Resize dock above the canvas", "Resize dock below the canvas", "Resize right sidebar", "Resize bottom dock" or "Resize outer right column". The splitter sits on the canvas side: after oleft, top, left and ctop; before cbottom, right, bottom and oright. It shows only when the zone is non-empty. | L144-146, L178-180, L213-215, L248-250, L385-387, L420-422, L454-456, L488-490, L3389 |
| 2.1.7 | Zone resize | Drag delta: oleft +dx, left +dx, right −dx, oright −dx, top +dy, ctop +dy, cbottom −dy, bottom −dy. The result is clamped to LIMITS. While a drag runs, window-level capture listeners follow the pointer, so fast drags are not lost. `buttons===0` ends the drag. Ending a resize marks the layout " (modified)". | L3251-3290, L3395-3398 |
| 2.1.8 | Empty zones | A zone with no groups is **hidden unless a panel drag is in progress** (`show: !empty \|\| dragging`). During a drag every empty zone appears at 44px with a dashed placeholder (1px dashed `#3A3F48`, radius 6, `#7D8590`, 12px). Vertical zones show vertical-rl text. Placeholder texts: "Drop here: outer left", "Drop here: full-width top", "Drop here: left", "Drop here: above the canvas", "Drop here: below the canvas", "Drop here: right", "Drop here: full-width bottom", "Drop here: outer right". | L118-120, L152-154, L187-189, L222-224, L389-391, L424-426, L458-460, L492-494, L3385-3388 |

### 2.2 Panel groups, tabs and title strips

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 2.2.1 | Group model | `{gid, tabs[], active, weight (default 1), collapsed?}`. gids are sequential `g100`, `g101`, … | L1179-1180 |
| 2.2.2 | Group box | bg `#16191E`, 1px `#262A31` border, radius 6, overflow hidden. Flex is `weight 1 0px`, or `0 0 auto` when collapsed. min-size along the stacking axis is 70px in vertical zones and 160px in horizontal ones (0 when collapsed). | L125, L159, L3303-3306 |
| 2.2.3 | Title/tab strip | `.strip`: min-height 32px, bg `#16191E` (`#22324A` while it is a tab-drop target), bottom border 1px `#262A31` (none when collapsed). The tab row scrolls horizontally (gap 2, padding 0 4px). | L126-127, L3313, L3324 |
| 2.2.4 | Tab styling | Uppercase, 12px, weight 600, letter-spacing 0.4px, padding 7px 8px, `white-space: nowrap`, `cursor: grab`. Active tab colour `#E6E8EB`, inactive `#7D8590`, hover `#E6E8EB`. The **underline** is 2px in accent **only on the active tab of a multi-tab group**. A lone panel has no underline. | L129-130, L3352-3355 |
| 2.2.5 | Tab tooltips | Single-tab group: "Click to collapse or expand · drag to move · drop on another tab strip to group". Multi-tab group: "Drag to move · drop on another tab strip to group". | L3373 |
| 2.2.6 | Tab click | Clicking the active tab of a **single-tab group, in a zone with more than one group**, toggles collapse. In every other case the click activates the tab and un-collapses the group. | L3358-3366 |
| 2.2.7 | Collapse strip | A collapsed group shows only its strip, and its body is `display: none`. **In a horizontal zone** (top, ctop, cbottom, bottom) the collapsed strip becomes a narrow vertical bar: `writing-mode: vertical-rl`, `transform: rotate(180deg)`, `flex: 1 1 auto`. That gives a title rotated 90° that reads bottom-to-top. In vertical zones it stays a horizontal title bar. The other groups take the space. | L3304-3313 |
| 2.2.8 | Close buttons | Multi-tab groups: each tab has its own 18x18 "×" (13px). It is hidden until the tab is hovered and has `aria-label "Close <Title>"`. Single-tab groups: one 24x24 "×" (15px) at the right end of the strip (margin 0 4px). It is hidden until the strip is hovered. | L131-136, L3317 |
| 2.2.9 | Close-twice behaviour | The first click **arms** the button: class `armed` (opacity 1, bold) and colour `#FF5C63`. The tooltip changes from "Close <Title> (click twice)" to "Click again to close <Title>". A second click on the armed button removes the panel from the zones (an empty group disappears) and marks the layout " (modified)". The armed state clears after **3000 ms**, or when the pointer **leaves** the button. Only one panel is armed at a time (`closeArm`). | L3237-3246, L3318-3322, L3376-3379, L3472 |
| 2.2.10 | Mixer meter orientation toggle | When the active tab is the Audio mixer, a 22x22 icon button sits next to its title (always visible). Its icon is three vertical bars when the meters are vertical and three horizontal bars when they are horizontal. Tooltip and aria: "Meters: vertical · click for horizontal" or "Meters: horizontal · click for vertical". Clicking sets a global `mixOrient` override ('h' or 'v'). By default the meters are vertical in vertical zones and horizontal elsewhere. | L131, L3233-3235, L3369-3372 |
| 2.2.11 | Dividers between groups | A 6px transparent button with radius 3 and hover `#2B4A7A`, aria "Resize between panels". Cursor `row-resize` in vertical zones and `col-resize` in horizontal ones. It sits before every group except the first. Dragging trades weight between the two adjacent groups: `perPx = totalWeight / (len − 40)`, and each side keeps at least `total*70/len`. It does **not** mark the layout modified mid-drag, but the resize end does (see 2.1.7). | L122-124, L3343-3351, L3273-3283 |
| 2.2.12 | Body | `flex: 1 1 auto; overflow: auto; container-type: size`. It hosts `<dc-import name="Panel" kind="{active}">`. | L138-140 |

### 2.3 Drag-and-drop docking

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 2.3.1 | Start | Tabs use HTML5 drag (`draggable`). `dataTransfer` gets `text/plain` = panel id with `effectAllowed=move`. `dragging` becomes true on the next tick, which reveals the empty zones. | L3380 |
| 2.3.2 | Drop on a tab strip | The panel joins that group as a new tab and becomes active. The strip highlights `#22324A` while targeted. | L3327-3328, L3212 |
| 2.3.3 | Drop on a group body | The body is split in halves along the zone's stacking axis. The first half inserts a new group **before** this one and the second half **after** it. The insertion edge shows a **3px accent line**: top or bottom border in vertical zones, left or right border in horizontal ones. Dropping a lone panel onto its own body does nothing. | L3329-3342, L3325-3326 |
| 2.3.4 | Drop on a zone | Anywhere else in the zone (including the empty placeholder) appends a new group at the end. The zone shows an outline `2px dashed accent`, offset -2px. `dragleave` clears it only when the pointer actually leaves the zone. | L3391-3394 |
| 2.3.5 | Placement semantics | The panel is first removed from wherever it was (empty groups are deleted), then inserted. After a placement, `over` and `dragging` are cleared and the layout is marked " (modified)". `dragend` clears the state. | L3191-3219, L3381 |
| 2.3.6 | showPanel(id) | Used by menus and Panel. If the panel is docked, its group activates it and un-collapses. Otherwise it is added as a new group in its HOME zone. | L1956-1962 |
| 2.3.7 | HOME zones | scenes→left, objects→left, inspector→right, transform→right, filters→right, chat→right, ytstats→right, mixer→bottom, outputs→bottom, agent→right, transitions→bottom. | L901 |

### 2.4 Layouts

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 2.4.1 | Panel ids and titles | scenes "Scenes", objects "Scene objects", inspector "Inspector", transform "Transform", filters "Filters", chat "Live chat", ytstats "Stream stats", mixer "Audio mixer", outputs "Outputs", agent "Agent activity", transitions "Transitions". | L897-900 |
| 2.4.2 | **Editing** layout (the default at start) | left: [Scenes], [Scene objects]. right: [Inspector] (weight 2), [Filters, Transform] (one tabbed group with Filters active). bottom: [Audio mixer], [Outputs, Transitions] (Outputs active). oleft, oright, top, ctop and cbottom are empty. Sizes: oleft 260, oright 280, top 140, **left 300**, ctop 140, cbottom 170, **right 340**, **bottom 190**. | L1181-1184, L1207-1210 |
| 2.4.3 | **Live** layout | left: [Scenes] (size 260). cbottom: [Audio mixer], [Transitions] (size 180). right: [Live chat], [Stream stats, Outputs, Agent activity] (size 340). bottom is empty. Other sizes as in Editing. | L1185-1188 |
| 2.4.4 | Saved layouts list | Starts as [Editing, Live]. Layouts are kept in memory only, with no persistence. | L1210 |
| 2.4.5 | "(modified)" tracking | `current` gets " (modified)" appended once after: a placement, a close, a resize end or a View toggle. | L3202-3205 |

---

<a id="canvas-structure-and-rendering"></a>
## 3. Canvas: structure and rendering
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 3.1 | Canvas section | `section aria-label="Canvas"`, `position: relative; isolation: isolate`, `min-height: 160px`, bg `#0A0B0E`. Its inner margin area has padding `28px 28px 20px`. | L253-255 |
| 3.2 | Frame | `data-canvas`, tabindex 0, aria-label "Scene canvas, <scene>". Width `min(100cqw, 100cqh*16/9)`, aspect 16:9, bg `#141A22`, outline 1px `#2E333B`, centred, `touch-action: none`, `user-select: none`. The logical canvas is **1920 x 1080**. | L256, L906 |
| 3.3 | Item placement | Each item is absolutely positioned with left/top/width/height as percentages of 1920x1080. Transform is `rotate(rot deg)`. When rx, ry or z is set it becomes `perspective(persp/1920*100 cqw) translateZ(z…cqw) rotateX(rx) rotateY(ry) rotate(rot)`, with persp defaulting to 1200. `opacity` = source opacity/100 × transition alpha. `mix-blend-mode` comes from the blend map. | L258, L1635-1642 |
| 3.4 | Blend modes | Normal→normal, Additive→plus-lighter, Multiply, Screen, Overlay, Darken, Lighten, Difference, Exclusion. | L1095 |
| 3.5 | Crop rendering | `x/y/w/h` is the visible, post-crop box. `cl/cr/ct/cb` are the cropped amounts in canvas px. The content is drawn in an inner box offset by `-cl/w` and `-ct/h` and sized `(w+cl+cr)/w` by `(h+ct+cb)/h`, then clipped (`overflow: hidden`). | L262-263, L1645 |
| 3.6 | Crop ghost | When the single selected item is cropped, or crop mode is on, the full uncropped extent is drawn as a 1px dashed `#E8A33D` outline with fill `rgba(232,163,61,0.08)`. | L259-261, L1646 |
| 3.7 | Demo source art (simulated) | **display**: bg `#20303F` and a fake desktop: a title bar `#2B3F52`, a code window `#172330` with a `#34506E` outline and coloured line bars (`#4C8DFF`, `#3A5A7A`, `#7FD6A8`, `#E8A33D`), a side window, and the caption "Display 1 · 3840 × 2160" (`#7C95AE`, 1.4cqw). **webcam**: bg `#3A2F45` with a head and shoulders silhouette in `#7A6890` and the label "Webcam · Brio" (`#CDB9E3`). **lower**: bg `#0F1115`, an accent bar 0.6cqw wide, and the text "jeff · building a stream app in Rust" (1.7cqw). **alerts**: bg `#E8A33D`, "NEW FOLLOWER" (0.9cqw, 600) over "kat.dev" (1.4cqw, 600), text `#1A1206`. **logo**: a `#4C8DFF` circle with a white "J" (4.5cqw). **ticker**: bg `#16191E`, `#9AA1AC` text "Today: building the canvas editor · github.com/jeffmhopkins/JStream · thanks for the follows". | L264-302, L325-327 |
| 3.8 | Generic source art (new sources) | Used for any item whose base id is not a demo id. **text**: bg = prop `bg`, colour = prop `color`, size = `size/1920*100` cqw, label = prop `text`. **color**: a fill of the prop colour. **waveform**: an SVG polyline of 61 points (a fake sine) stroked in the prop colour at width 0.8, transparent bg. **spectrum**: 24 bars with radius 2px at the top, transparent bg. **Others**: bg `#1B222C` with a centred type icon (3cqw), the item name (1.2cqw) and the type name (0.9cqw `#7D8590`). | L303-324, L1620-1630 |
| 3.9 | Draw order | The objects tree is flattened top to bottom, filtered to `obj:` keys that have a transform and are not `hidden`, then reversed, so the canvas draws bottom to top. Audio-only sources (no xf) are not drawn. | L1780-1783 |
| 3.10 | Single-selection chrome | A 2px solid outline in the selection colour (accent, or `#E8A33D` in crop mode). A rotation stem (1px wide, 30px tall above the top edge), then a rotation knob: a 14px white circle with a 2px border in the selection colour at top −38px, tooltip "Drag to rotate · Ctrl: 45° steps · Shift: half speed". **8 handles** at TL, T, TR, R, BR, B, BL, L: 10px white squares with a 2px border and radius 2. In crop mode they become **12px** squares filled `#E8A33D` with a crosshair cursor. Otherwise cursors are nwse/ns/nesw/ew. Handle aria: "Resize top left" … (or "Crop …"). Handle tooltip: "Drag to resize (keeps aspect) · Ctrl: stretch · Alt: crop · Shift: half speed", or in crop mode "Drag to crop · Shift: half speed". | L338-346, L1602-1607, L1675-1682 |
| 3.11 | Readout pill | 10px below the item, centred. Selection-colour bg, `#0F1115` text, mono 11px, padding 2px 7px. At rest: `W × H`, plus "  · cropped" if cropped, plus "  · N°" if rotated. While moving: `X n  Y n`. While resizing: `W × H`. While rotating: `N°`. While cropping: `crop l / r / t / b`. | L345, L1652-1657 |
| 3.12 | Multi-select outline | The other selected items get a 2px solid outline with no handles. | L335-337, L1648 |
| 3.13 | Group selection box | When more than one item is selected: the bounding box with a 1px dashed accent outline at offset 4px and z-index 5. A readout 12px below in accent shows "N items · W × H", or during a group op the scale `W × H` or rotation `N°`. A rotation stem and a knob at −42px (tooltip "Drag to rotate the selection · Ctrl: 45° steps · Shift: half speed"). 8 handles at 10px (tooltip "Drag to scale the selection (keeps aspect) · Ctrl: stretch · Shift: half speed", aria "Resize selection <pos>"). | L349-358, L1685-1699 |
| 3.14 | Marquee | 1px solid accent border, fill `rgba(76,141,255,0.12)`. | L359-361 |
| 3.15 | Snap guides | Full-height or full-width 1px lines in `#FF4FD8`, shown only during a drag. | L362-367 |
| 3.16 | Hover outline | 1px at `rgba(138,180,255,0.6)` on hover, via CSS. | L330, L39 |
| 3.17 | Cursor | `move` on selected items and `pointer` on the rest. | L1651 |
| 3.18 | Source tags (inactive) | Markup exists for a dashed outline and a coloured name tag (`it.tagged` / `srcTag`), but `look.tagged` is always false. This is dead UI. | L331-334, L1615 |
| 3.19 | Scene caption | Bottom-left (left 12, bottom 6), mono 11px `#5A606B`: "<scene> · 1920 × 1080". During a transition it appends "  · move…" / "  · fade…" (the lowercased kind followed by an ellipsis). | L379, L3417 |
| 3.20 | Keyframe banner | Shown when the current scene is a keyframe. Top-centre (top 10), pill (radius 12, padding 4px 10px), bg `rgba(176,124,255,0.16)`, border `#6E4FB0`, text `#D9C6FF` 11px: "◆ Keyframe of “<parent>”: only placement and visibility can change". | L381, L3416 |
| 3.21 | Privacy overlay | While privacy is on, the canvas frame gets `filter: blur(14px) brightness(0.6)`. A centred notice reads "Privacy screen on · output blurred · mic muted" (bg `rgba(122,30,35,0.92)`, border `#E5484D`, white, weight 600, radius 8, padding 10px 16px). **Simulated: there is no real mic mute.** It is also exposed to Panel as `master.privacy`. | L380, L3440, L2254 |

<a id="canvas-interactions"></a>
## 4. Canvas: interactions
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 4.1 | Click to select | A primary-button press on an item selects it alone, unless it is already in the selection, in which case the whole selection is kept and dragged. Then a move op starts. | L1658-1671 |
| 4.2 | Shift-click | Shift-click on an unselected item adds it and starts a move of all selected items. Shift-click on a selected item removes it and starts no drag. When the last item is removed, the primary selection becomes the scene. | L1661-1666, L1334-1338 |
| 4.3 | Disabled during transitions | Item presses are ignored while a transition animates. Handles and selection outlines are hidden during the animation. | L1611-1612, L1659 |
| 4.4 | Drag threshold | Nothing moves until the pointer travels 2 screen px. Pointer capture is set on the canvas element (not the handle) and the canvas is focused. | L1376-1381, L1405 |
| 4.5 | Shift: half speed | Any op except the marquee uses a virtual pointer that moves at 0.5× while Shift is held. | L1386, L1401-1403 |
| 4.6 | Move | Translates all selected items. **Snapping** is on when `snap` is true, Ctrl/Cmd is **not** held, and no moved item is rotated. It snaps the selection bbox's left/centre/right and top/middle/bottom to candidates: canvas edges and centre (0, 960, 1920 / 0, 540, 1080), plus the edges and centres of every other unrotated drawn item. The threshold is 6 screen px. The nearest snap per axis wins, and a guide line is shown at the snapped coordinate. | L1361-1371, L1409-1440 |
| 4.7 | Resize (single) | Handles resize in the item's own rotated axes. **Aspect is kept by default; Ctrl stretches.** A corner follows whichever axis snapped, otherwise the larger relative change. Minimum size is 10px. The opposite side stays anchored. Edge snapping applies only to unrotated items, on the moving edge. | L1488-1539 |
| 4.8 | Crop (single) | Alt-drag on a handle, or any handle drag in crop mode, crops. The visible box shrinks or grows on that side, limited to the original extent (it cannot uncrop beyond `w+cl` and so on). Crop values update and the content does not scale. | L1494-1500, L1533-1538, L1681 |
| 4.9 | Rotate (single) | Drag the knob. Angle = atan2 from the item centre + 90°. **Ctrl: 45° steps.** With snapping on (and no Ctrl), it snaps to 0/90/180/−90/−180/270 within 3°. Normalised to (−180, 180] and rounded to 0.1°. | L1479-1487 |
| 4.10 | Group scale | Scales all selected items from the opposite side of the bbox (edge handles scale from the centre on the other axis). Aspect is kept (the larger factor for corners) unless Ctrl is held. Minimum bbox is 20px. Positions, sizes **and crop amounts** scale. | L1441-1459 |
| 4.11 | Group rotate | Rotates item centres and adds the delta to each item's rotation around the bbox centre. Ctrl gives 45° steps. Rotations are normalised to [−180,180). | L1460-1478 |
| 4.12 | Marquee select | A press on empty canvas, or on the dark margin around the frame, starts a box select. Hits use an unrotated bbox-intersection test. Shift adds to the previous selection. A click with no movement and no Shift clears the selection, which then becomes the scene. A press on empty canvas also closes any open context menu. | L1413-1424, L1549, L1732-1745 |
| 4.13 | Double-click item | Opens the Properties dialog for the item. A reference opens its source's properties. | L1673, L2030-2046 |
| 4.14 | Right-click | On an item, opens the context menu with scope `canvas` and the item key, and selects the item first if it was not selected. On empty canvas, opens scope `canvas` with no key and resets the selection to the scene. On the dark margin, opens scope `canvasOuter`. | L1674, L1746-1751, L2843-2859 |
| 4.15 | Op teardown | Releasing anywhere ends the op: pointerup or pointercancel in the capture phase, window blur, `lostpointercapture`, or a move with `buttons === 0`. Guides and the marquee are cleared. | L1395-1400, L1544-1583 |
| 4.16 | Keyboard on the canvas (canvas focused) | **C** toggles crop mode. **V** selects the move tool (leaves crop mode). **Esc** clears the selection (to the scene), leaves crop mode and closes the context menu. **Ctrl/Cmd+A** selects all drawn items. **Arrow keys** nudge selected items by 1px, or **10px with Shift**. | L1754-1767 |
| 4.17 | Floating canvas toolbar | Top-right of the canvas section (right 8, top 8, z-index 5). role=toolbar "Canvas tools". bg `#1A1D23`, border `#2E333B`, radius 8, padding 3, gap 2. Buttons are 30x30 with radius 5 and a 17px stroke icon. Order: **Move and resize** (aria-pressed when not in crop mode; tooltip "Move / resize (V) · Shift: half speed or add to selection · Ctrl: no snap while moving, stretch on handles · Alt on a handle: crop"); **Crop** (toggles; tooltip "Crop (C) · drag a handle to trim"); then a 1x22 `#2E333B` separator; then **Snapping** (toggles; tooltip "Snapping on: edges, centers and other items · click to turn off" or "Snapping off · click to turn on"). Active colours: Move and Snapping use bg `#22324A` and icon `#B5CEFF`. Crop uses bg `#4A3510` and icon `#F5C77E`. Inactive buttons have a transparent bg and icon `#9AA1AC`. A pointerdown on the toolbar stops propagation. | L371-378, L1716-1726 |
| 4.18 | Defaults | Snap on. Crop mode off. Initial selection `obj:webcam` (selIds ['webcam']). alignTo 'selection'. | L1233-1234 |
| 4.19 | Unused canvas hints | `cv.hint`, `snapText`, `snapBorder`, `snapColor` and `selectBg`/`cropBg` are computed but no template renders them. Hint strings: "Crop: drag a handle to trim · C to leave crop mode" / "Shift-click: add/remove · drag to move all · Transform panel: align and distribute" / "Shift: half speed / add to selection · Move: Ctrl no snap · Handles: keep aspect, Ctrl stretch, Alt crop" / "Click to select · Shift-click to add · drag on empty canvas to box-select". | L1704-1731 |
| 4.20 | Privacy key | **Pause** anywhere in the window toggles privacy. | L1572 |

---

<a id="context-menus-and-submenus"></a>
## 5. Context menus (right-click) and submenus
<!-- @section
-->

### 5.1 Menu mechanics

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 5.1.1 | One global menu | Fixed position at the pointer, z-index 100, min-width 240, max-width 360, padding 5, bg `#1F232A`, border `#2E333B`, radius 8, shadow `0 12px 32px rgba(0,0,0,0.55)`, 13px text. role=menu, aria "Context menu". | L861-877 |
| 5.1.2 | Item anatomy | Height 28, padding `0 10px 0 8px`, radius 5. Columns: a 14px check column (✓, `#8AB4FF`, 12px); an 8px round colour swatch (transparent unless set); the label (ellipsised); a shortcut hint (mono 11px `#7D8590`, padding-left 12); a 10px submenu arrow "▸" (`#9AA1AC`). Disabled items have opacity 0.4 and get no hover highlight. | L866-873, L2891 |
| 5.1.3 | Separators and heads | Separator: 1px `#2E333B` with margin 4px 2px. Head: uppercase, 11px, weight 600, letter-spacing 0.6px, `#7D8590`, padding `6px 10px 2px 30px` (aligned with the labels). | L864-865 |
| 5.1.4 | Positioning | If `x + 300 > window width`, the menu opens at `x − 300` (minimum 4). If the estimated height (item 30, head 24, separator 9, +12) would overflow, it is moved up to `H − h − 4` (minimum 4). | L3081-3085 |
| 5.1.5 | Submenu | Width 250, z-index 101, same styling. Items are `menuitemradio` with a check, a label and a hint (no swatch). It opens **on hover** over an item with a submenu, or toggles on click. Hovering any other item closes it. Windows-style placement: flush with the parent menu's right edge (−2px), first item level with the opener (top −6). It flips left when there is no room and is clamped vertically. The opener keeps the `.on` highlight while its submenu is open. | L878-892, L3067-3096, L2620-2625 |
| 5.1.6 | Keyboard | On open, the first enabled item is focused. Hovering an item also focuses it. Main menu: ↑/↓ wrap, Home/End, → opens a submenu and focuses its first item, ← closes the submenu, Esc and Tab close everything. Submenu: ↑/↓ wrap, ← returns to the opener, Esc and Tab close. | L2854-2858, L3100-3122 |
| 5.1.7 | Closing | Choosing an item closes the menu. So does any press outside it (root `closeMenus`), a press on the canvas, and Esc on the canvas. | L2890, L3424-3427 |

### 5.2 Menu contents (exact order; "(dis)" = permanently disabled, hint in [ ])

**Shared sub-blocks**
- **Pop out ▸** submenu (L1941-1954): "Source in a window" (objects) or "Scene in a window" (scenes); then the head "FULL SCREEN ON A MONITOR"; then "Display 1 · primary" [3840×2160] and "Display 2 · MacroSilicon USB" [1920×1080].
- **Transition in ▸** [current override name, or "Default"] and **Transition out ▸** [same] (L2895-2898, L2627-2631). The submenu (L2634-2651) reads: "Default (Smooth move)" (✓ when there is no override); head "OVERRIDE"; "Smooth move" [Move 600 ms]; "Quick fade" [Fade 300 ms]; "Slow fade" [Fade 900 ms]; "Cut" [Cut] (✓ on the active override); separator; "Manage presets…", which opens or shows the Transitions panel. Choosing a preset sets `trans.into[scene]` or `trans.outOf[scene]`, and choosing Default deletes the override.
- **objectItems** (L2906-2922): Bring to front · Bring forward · Send backward · Send to back · sep · Center on canvas · Fit to canvas · Stretch to canvas · Reset rotation · Clear crop · sep · **Crop** / **Leave crop mode** [C] (disabled when more than one is selected) · Duplicate as reference [Ctrl+D] (single obj only) · Duplicate as clone [Ctrl+Shift+D] (single obj only) · Remove from scene [Del] (dis).

| # | Scope | Items | Lines |
|---|---|---|---|
| 5.2.1 | Canvas margin (`canvasOuter`) | Pop out ▸ (current scene) · sep · Select all [Ctrl+A] · "Turn snapping off"/"Turn snapping on" · sep · Transition in ▸ · Transition out ▸ (current scene) · sep · Scene properties… · Edit scene filters… (selects the scene and shows the Filters panel) | L2924-2936 |
| 5.2.2 | Empty canvas (`canvas`, no key) | Select all [Ctrl+A] · Turn snapping off/on · sep · Add source… · Paste [Ctrl+V] (dis) | L2937-2945 |
| 5.2.3 | Empty space in Scenes panel | New scene · New folder · sep · Paste scene [Ctrl+V] (dis) | L2947-2954 |
| 5.2.4 | Empty space in Scene objects panel | Add source… · Add scene as source… (opens Add source with the Scene type) · New group · sep · Select all [Ctrl+A] · Paste [Ctrl+V] (dis) | L2955-2964 |
| 5.2.5 | Folder (`f:`) or group (`grp:`) row | Scenes panel: New scene. Objects panel: Add source… (into the folder). Then New folder / New group · Rename [F2] (dis) · sep · "Delete folder" / "Ungroup" [Del] (dis) | L2966-2976 |
| 5.2.6 | Keyframe row in Scenes | Switch to it · Rename [F2] (dis) · sep · Transition in ▸ · Transition out ▸ · sep · Reset to “<parent>” · New keyframe · sep · Delete keyframe [Del] (enabled) | L2977-2990 |
| 5.2.7 | Scene row in Scenes | Show in preview (dis) · Rename [F2] (dis) · sep · Transition in ▸ · Transition out ▸ · sep · Properties… [Double-click] · Edit filters… · Pop out ▸ · sep · New scene · New keyframe · New folder · sep · Duplicate scene (dis) · Delete scene [Del] (dis) | L2991-3009 |
| 5.2.8 | Nested scene in Scene objects | Properties… [Double-click] · Edit filters… · Pop out ▸ · sep · ✓Audio (check, toggles "Include its audio") · Add source… (into it) · sep · Open nested scene (dis) · Remove from scene [Del] (dis) | L3010-3023 |
| 5.2.9 | Audio-only source (no picture) | Properties… [Double-click] · Edit filters… · sep · [✓Audio · sep] · Remove from scene [Del] (dis) | L3026-3034 |
| 5.2.10 | Visual object, single | [✓Audio · sep, if it can carry audio] · Properties… [Double-click] · Edit filters… · Pop out ▸ · sep · [if a duplicate: head "DUPLICATE OF <base name>", ✓"Reference to original", ✓"Independent clone", sep] · [if a demo cross-scene ref (`obj:webcam`): head "SOURCE", ✓Reference to original, ✓Independent clone, sep] · objectItems · then **canvas scope**: Select all [Ctrl+A] (appended directly with no separator); **objects scope**: sep · Add source… · New group | L3035-3054 |
| 5.2.11 | Visual objects, multi | head "N ITEMS SELECTED" · Group selection · sep · [✓Audio only if a single item is selected, so not shown here] · objectItems (Crop and Duplicate items disabled) · then the canvas or objects tail as in 5.2.10 | L3036, L3024-3025 |
| 5.2.12 | Layer actions | These operate on the selection, top item first (front and backward are reversed). They move within the object's own tree list: front = index 0, back = end, forward = −1, backward = +1. | L2901-2904, L1832-1846 |
| 5.2.13 | Shortcut hints are labels only | Ctrl+D, Ctrl+Shift+D, Del, F2 and Ctrl+V have **no key handlers**. Only C, V, Esc, Ctrl+A, the arrows, Pause and the F-key hotkeys are implemented. | — |

---

<a id="dialogs-and-floating-windows"></a>
## 6. Dialogs and floating windows
<!-- @section
-->

### 6.1 Create new stream (YouTube)

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 6.1.1 | Shell | Modal at z-index 200, 460px wide, bg `#1A1D23`, radius 10, padding 18, gap 14. Header "Create new stream" (16px, 600) and a × (28px). | L521-527 |
| 6.1.2 | Fields (in order) | **Title** (placeholder "What are you streaming?"). **Description** (textarea, 3 rows, vertical resize, placeholder "Optional"). **Visibility** segmented control: Public / Unlisted / Private, default Unlisted. **Latency**: Normal / Low / Ultra-low, default Low, note "Lower latency means quicker chat replies, but less buffering for viewers." **Start**: Start now / Schedule, default Start now. **Category** select: Science & Technology (default), Gaming, Education, People & Blogs, Music. **Start time** (`datetime-local`), shown only when Start = Schedule. Toggles: **"Allow viewers to rewind (DVR)"** (default on) and **"Made for kids"** (default off). | L528-562, L1232, L1873-1888 |
| 6.1.3 | Segmented control style | Container bg `#0F1115` with a `#2E333B` border, radius 6, padding 2. Options are flex-equal with radius 4. The selected option has bg `#2B313A`. Note text is 11px `#7D8590`. | L534-544 |
| 6.1.4 | Buttons | Cancel and **Create stream** (primary). Create stream is disabled, at opacity 0.45, until the title is non-blank. | L563-566, L1893-1894 |
| 6.1.5 | Submit | Prepends a broadcast `{title, privacy, when, latency}` and selects it. `when` is "Now", or the datetime with "T" replaced by a space when scheduled. The draft resets every time the dialog opens. **Simulated: no API call.** | L1895-1905 |
| 6.1.6 | Keys | Esc closes. Enter clicks the primary button unless focus is in a TEXTAREA, BUTTON or SELECT. A press on the backdrop closes. | L2183-2189, L1906-1909 |

### 6.2 Properties dialog (sources and scenes)

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 6.2.1 | Shell | Modal at z-index 210, **860 × 640** (clamped to 100%), radius 10. Header: a 20px type icon, the title (15px 600) and subtitle (11px `#9AA1AC`), and a ×. The body is a 2-column grid `1.15fr \| 1fr`. Footer: **Defaults** on the left, then **Cancel** and **OK** (primary) on the right. | L591-601, L678-683 |
| 6.2.2 | Left column | **Name** text field (prefilled with the display name), then the schema fields. Each field has an optional note (11px `#7D8590`). Field types: text, textarea, select, number stepper (clamped to min/max, step), switch, colour swatches (8 colours: `#FFFFFF #E6E8EB #0F1115 #4C8DFF #3FB27F #E8A33D #E5484D #B07CFF`). | L603-652, L968, L2053-2068 |
| 6.2.3 | Draft semantics | Edits go to a draft. **OK** saves the values and the name (a blank name is ignored). **Cancel**, Esc or the backdrop discards. **Defaults** resets the values to the schema defaults (not the per-item PROP_DEFAULTS) and keeps the name. | L2069, L2138-2167 |
| 6.2.4 | Opening rules | Opened by double-click, by "Properties…" in menus, and by Panel. A reference opens its source's properties. Objects cannot be opened inside a keyframe (a toast explains why). After Add source creates a source, its properties open at once ("Like OBS"). | L2030-2046, L2452-2453 |
| 6.2.5 | Scene extras | Scene dialogs start with **Transition in** and **Transition out** selects. Options: "Default (<default preset>)" followed by every preset name. Notes: "Used when switching into this scene." and "Used when leaving it, unless the next scene has its own “in”." On OK these are written to `trans.into` and `trans.outOf` rather than to the scene props. | L2017-2028, L2039-2044, L2153-2160 |
| 6.2.6 | Right column: FILTERS | Bg `#16191E` with a left border. Head "FILTERS" (11px 600, letter-spacing 0.8) plus a note: "whole scene" for scenes, "everywhere this source appears" for sources. Chains: **VIDEO** (monitor icon) and/or **AUDIO** (speaker icon), by target type (see 6.4.6). Each filter row is 30px high on bg `#1F232A` with radius 5. It contains: an eye toggle (24px; on: eye `#C9CED6`; off: eye-slash `#5A606B`; aria "Turn off <name>" / "Turn on <name>"); the name (`#E6E8EB`, or `#7D8590` with line-through when off); a summary (at most 50% width, 12px); and a remove × ("Remove"). Row tooltip: "Double-click for its settings · drag to reorder". Empty-chain texts: "No video filters." and "No audio filters." (`#5A606B`). | L653-671, L2070-2126 |
| 6.2.7 | Filter summary rule | Scene-input filters show the scene name in italic `#B5CEFF`, or "no scene" in `#E8A33D`. A filter with exactly one number shows that value with its unit. Otherwise it shows up to two changed numbers as "Label value · Label value", or "defaults" (mono `#9AA1AC`). Minus signs are rendered as "−". | L2079-2088 |
| 6.2.8 | Reorder | HTML5 drag within the same chain only. The drop line is an inset 2px `#4C8DFF` on the top or bottom half. | L2095-2121 |
| 6.2.9 | Live filter edits | Filter changes apply immediately and are not part of OK/Cancel. Footer text: "Changes apply right away. Fine-tune settings in the Filters panel." Button **"+ Add filter…"** (primary style). Double-clicking a row opens the Filter settings dialog on top. | L672-675, L2094, L2130 |
| 6.2.10 | Schemas (title / subtitle; fields with default) | **display** "Display capture properties" / "Windows Graphics Capture": Display [Display 1 · 3840×2160 (primary) \| Display 2 · MacroSilicon USB · 1920×1080]; Capture the mouse cursor (on); Hide the yellow capture border (on, note "Needs Windows 11."); Tone-map HDR to SDR (off). **device** "Video capture device properties" / "Media Foundation": Device [Logitech BRIO \| Integrated camera \| JStream virtual camera]; Resolution [3840×2160 \| 1920×1080* \| 1280×720]; Frame rate [60* \| 30 \| 24]; Video format [MJPEG* \| NV12 \| YUY2] (note "MJPEG is decoded on the GPU."); Buffer frames (smoother, adds latency) (off); Turn the camera off when not shown (on); Use this source's audio (off, note "When off, it doesn't appear in the Mixer."). **browser** "Browser source properties" / "Embedded Chromium (CEF)": URL "https://streamlabs.com/alert-box/…"; Width 800 px (1-8192, step 10); Height 600 px; Frame rate 60 fps (1-120); Custom CSS "body { background: transparent; margin: 0; }"; Use this source's audio (on); Shut down when not visible (off); Refresh when the scene becomes active (off). **text** "Text properties" / "Text source": Text (area) "Text"; Font [IBM Plex Sans* \| IBM Plex Mono \| Segoe UI \| Consolas]; Size 34 px (6-400); Color #E6E8EB; Background #0F1115; Outline (off). **image** "Image properties" / "Image source": Image file "C:\Users\jeffm\Pictures\jstream-logo.png"; Unload when not showing (off); Apply alpha in linear space (off). **scene** "Scene properties" / "Scene": [Transition in, Transition out]; Hotkey (text); Background color #0F1115; Show in multiview (on); Include its audio when nested (on, note "When off, its sources don't reach the Mixer through this scene."). **window** "Window capture properties" / "Windows Graphics Capture": Window [Visual Studio Code · JStream* \| Firefox · YouTube Studio \| Factorio]; If the window closes, match a new one by [Title, then executable* \| Executable only \| Title only]; Capture the mouse cursor (on); Client area only (no title bar) (on). **spout** "Spout2 receiver properties" / "Shared D3D11 texture": Sender [First available* \| TouchDesigner · out1 \| Resolume Arena]; Use the sender's alpha (on). **audioin** "Audio input properties" / "WASAPI": Device [ES-9 · Line 1/2* \| Logitech BRIO microphone \| Default input]; Downmix to mono (on); Use this source's audio (on). **desktop** "Desktop audio properties" / "WASAPI loopback": Playback device [ES-9 · Speakers \| Default playback device*]; audio (on). **appaudio** "Application audio properties" / "WASAPI process loopback": Application [Spotify* \| Discord \| Firefox \| Factorio]; Include its child processes (on); audio (on). **media** "Media file properties" / "Media Foundation decode": File "C:\Users\jeffm\Videos\intro.mp4"; Loop (on); Restart when it becomes visible (on); Hardware decoding (on); Close the file when not visible (off); audio (on). **color** "Color properties" / "Solid fill": Color #4C8DFF. **waveform** "Audio waveform properties" / "Audio visualiser": Audio source [Mic · ES-9 Line* \| Desktop audio \| Alerts]; Style [Line* \| Filled \| Mirrored]; Time window 50 ms (5-1000, step 5); Color #3FB27F. **spectrum** "Audio spectrum properties" / "Audio visualiser": Audio source [default Desktop audio]; Bands 32 (4-256); Frequency scale [Logarithmic* \| Linear]; Falloff [Slow \| Medium* \| Fast]; Color #4C8DFF. Types without a schema fall back to image. | L993-1086, L2005-2008 |
| 6.2.11 | Per-item defaults | obj:lower text "jeff · building a stream app in Rust". obj:ticker text "Today: building the canvas editor · github.com/jeffmhopkins/JStream", size 20, color #9AA1AC, bg #16191E. obj:alerts w 280, h 120. Scene hotkeys: Starting Soon F1, Desktop + Cam F2 (also `transition: 'Fade'`, which is unused), Just Chatting F3, BRB F4, Ending F5. | L1088-1094 |

### 6.3 Filter settings dialog

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 6.3.1 | Shell | Modal at z-index 225 (above Properties), 460px wide. Header: an 18px icon (monitor for video, speaker for audio), the filter name, sub "Video filter on “<target>”" or "Audio filter on “<target>”", an **on/off switch** (34x18, knob 14px at 18/2, aria "Filter on"), and a ×. | L688-699, L2223-2227 |
| 6.3.2 | Body | A hint line (12px `#9AA1AC`) with the type's hint, then a row per param: the label in a 132px column, then the control. Number stepper (max 200px; values clamped to min/max and rounded to `dec` places, or 2 when step < 1). Select. Scene select (first option "Choose a scene…", then the allowed scenes). Text (mono 12). Colour swatches (the param default plus the 8 standard colours, de-duplicated). With no params: "This filter has no settings." | L700-733, L2209-2221 |
| 6.3.3 | Live preview | Every change writes straight into the filter. **Cancel**, Esc or the backdrop **restores the original**. **OK** keeps the changes. **Defaults** clears the params (a scene-input filter keeps its scene). **Remove** (text `#FF8A8F`) deletes the filter and closes. | L734-740, L2205-2234 |
| 6.3.4 | Blocking | Filters on objects cannot be edited while in a keyframe. | L2191-2196 |

### 6.4 Add filter dialog

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 6.4.1 | Shell | Modal at z-index 220, **720 × 520**. Header "Add filter" with sub "For “<target name>”". The body is two equal columns. Footer: Cancel and **Add filter** (primary; disabled at 0.45 when the type needs a scene and none is chosen). | L745-801 |
| 6.4.2 | Left: search and list | A search box (placeholder "Search filters") that matches name, hint and group, case-insensitive. The listbox shows chain heads (VIDEO/AUDIO with icon, 11px 700, letter-spacing 0.8, `#C9CED6`), then group subheads (11px `#7D8590`), then type rows (selected row bg `#22324A`). No matches: "No filters match." The first type allowed for the target is preselected. | L756-772, L2261-2266, L2293-2321 |
| 6.4.3 | Right: detail | The name (15px 600), "<Video|Audio> · <group>", and the hint (`#C9CED6`). For scene-input filters: a select labelled "Uses the scene" ("Choose a scene…" plus the allowed scenes) and the note "The scene shows nested under the object in Scene objects." "Settings" lists the param labels joined with " · ". Footer note: "Scene filters apply to the whole scene." or "Applies everywhere this source appears." | L774-795, L2322-2336 |
| 6.4.4 | Add behaviour | Appends `{id, type, on: true, p}` to the target. A filter with **more than 3 params opens the Filter settings dialog immediately**. Others set `fltNew` (passed to Panel to highlight the new row). Double-clicking a non-scene-input type adds it at once. The Properties dialog stays open beneath. | L2296-2307, L2317 |
| 6.4.5 | Allowed scenes | Excludes the current scene, the target scene itself, any scene that contains the target in the objects tree, and keyframes of excluded scenes (no loops). | L2268-2285 |
| 6.4.6 | Chains per target | `out:program` gets video and audio. Other outputs get their own chains (Virtual camera is video only). Scenes get video. Sources get video unless they are audio-only, plus audio when they are audio or have audio. | L2173-2179 |
| 6.4.7 | Filter catalogue (kind / group / name / hint / params) | **VIDEO.** *Color and keying*: Color correction "Gamma, contrast, saturation, hue" (Gamma 0 [-3..3, 0.05], Brightness 0 [-100..100], Contrast 0, Saturation 0, Hue shift 0° [-180..180], Color multiply #FFFFFF). Apply LUT "A .cube or .png colour look" (LUT file "C:\LUTs\warm-film.cube", Amount 100%). Chroma key "Remove a green or blue screen" (Key color [Green\|Blue\|Magenta\|Custom], Similarity 40%, Smoothness 8%, Spill reduction 10%). Color key "Remove any one colour" (Color #3FB27F, Similarity 30%, Smoothness 5%). Luma key "Remove dark or bright areas" (Luma min 0%, Luma max 100%, Smoothness 4%). *Detail*: Sharpen "Crisper detail" (Amount 0.2 [0..1, 0.05]). Blur "Gaussian, whole image or an area" (Size 8px [0..200], Area [Whole image\|Rectangle], Area X 0px, Area Y 0px). *Style*: Rounded corners "Round the corners, optional border" (Radius 16px [0..500], Border 0px [0..50], Border color #FFFFFF). Drop shadow "Soft shadow behind" (Offset X 6px, Offset Y 8px [-200..200], Blur 16px, Opacity 40%, Color #0F1115). Outline "A stroke around the shape" (Width 3px, Color #FFFFFF). Glow "Soft light around the shape" (Size 20px, Strength 60%, Color #4C8DFF). *Motion and timing*: Scroll "Continuously scroll the image" (Speed X 80 px/s, Speed Y 0 px/s [-2000..2000]). Video delay "Hold the picture back to match audio" (Delay 0 ms [0..5000, step 10]). *Uses another scene*: Alpha mask "Cut out by another scene’s shape" (Mask scene, Invert [No\|Yes]). Luma matte "Bright areas of a scene show this" (Matte scene, Invert). Clip to "Only show inside another scene" (Clip scene). Displacement "Warp by another scene’s brightness" (Map scene, Strength 20px). *Advanced*: Custom shader "Your own WGSL effect, time-limited" (Shader file "effects\crt.wgsl", Strength 50%). **AUDIO.** *Levels and dynamics*: Gain "Turn it up or down" (Gain 0 dB [-30..30, 0.5]). Compressor "Even out loud and quiet" (Ratio 4 :1 [1..32], Threshold -18 dB, Attack 6 ms, Release 60 ms, Output gain 0 dB, Duck under [Nothing\|Mic · ES-9 Line\|Desktop audio]). Limiter "Never go over a level" (Threshold -6 dB, Release 60 ms). Noise gate "Silence below a level" (Open threshold -26 dB, Close threshold -32 dB, Attack 25 ms, Release 150 ms). *Cleanup*: Noise suppression "RNNoise: remove fans and hum" (Amount 100%). Invert polarity "Flip the waveform" (none). *Tone*: EQ "Shape low, mid and high" (Low/Mid/High 0 dB [-20..20], Mid frequency 1000 Hz [100..8000, 10]). *Timing*: Audio delay "Hold the sound back to match video" (Delay 0 ms, step 5). *Mastering*: Loudness target "Ride the level to a LUFS target" (Target -14 LUFS [-30..-6], Max boost 12 dB, Speed [Slow*\|Medium\|Fast]). True-peak limiter "A brick wall the encoder can’t clip" (Ceiling -1 dBTP [-12..0, 0.1], Release 50 ms). Ducking "Dip everything else while the mic talks" (Triggered by [Mic · ES-9 Line\|Desktop audio], Amount -12 dB, Release 400 ms). *Plugins*: CLAP plugin "Host a CLAP audio effect" (Plugin [TDR Nova\|Airwindows Consolidated\|Surge XT Effects]). | L1117-1155 |
| 6.4.8 | Demo filters | webcam: Color correction (sat 6), Rounded corners (24), Alpha mask (off, Cam shape), Drop shadow. display: Sharpen. alerts: Chroma key (off), Gain −6, Limiter. ticker: Scroll. logo: Displacement (Lower third pack, 12), Glow. **out:program**: Loudness target, Compressor (2:1, −12), True-peak limiter, Apply LUT (off, stream-look.cube, 40). out:youtube: Sharpen 0.15. scene Desktop + Cam: LUT 60. Intro overlay: CC (contrast 12, sat −8), Glow (off, 30), Clip to (Cam shape). Just Chatting: LUT teal-orange 45, Sharpen 0.1. BRB: Blur 24. | L1156-1173 |

### 6.5 Add source dialog

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 6.5.1 | Shell | Modal at z-index 210, 720px wide. Header "Add source" with sub "Into “<parent or current scene>”". Two columns `1fr \| 1.1fr`. Footer: Cancel and the primary button labelled **Create** (new) or **Add** (existing/nest), disabled at 0.45 when nothing is chosen. | L805-857, L2413-2429 |
| 6.5.2 | Type list | A listbox of category heads (11px 600, letter-spacing 0.8, `#7D8590`, upper-case), then rows with a 16px icon and a name (selected row bg `#22324A`). **CAPTURE**: Display capture, Window capture, Video capture device, Spout2 receiver. **AUDIO**: Audio input, Desktop audio, Application audio. **MEDIA AND WEB**: Media file, Image, Browser. **GENERATED**: Text, Color, Audio waveform, Audio spectrum. **SCENES**: Scene. The default type is Display capture. | L816-826, L972-988 |
| 6.5.3 | Descriptions | Display "A whole monitor, through Windows Graphics Capture." · Window "One application window." · Device "A webcam or capture card." · Spout2 "Video shared by another app, such as TouchDesigner or Resolume." · Audio input "A microphone or line input." · Desktop audio "Everything a playback device plays." · Application audio "Sound from one application." · Media file "A video or audio file, with looping." · Image "PNG, JPG, WebP or animated GIF." · Browser "A web page or widget." · Text "Styled text." · Color "A solid fill. Handy as a mask or matte." · Waveform "Draws an audio source as a moving wave." · Spectrum "Draws an audio source as frequency bars." · Scene "Nest another scene inside this one." | L973-987 |
| 6.5.4 | Right pane | A 22px icon, the type name (600) and the description. A **"Create new"** radio with a Name input beneath it, indented 22px (focusing or typing selects Create new). Below: "Or add an existing <type name lowercased> as a reference" with italic radio rows, one per distinct existing source of that type. For **Scene**: no Create new; the list is "Nest a scene" (scenes that are not current and not already nested). Notes: scene "A nested scene is a reference: edit it in its own scene." or "No other scenes can be nested here."; audio types "Audio only: it shows in the Mixer, not on the canvas." Radios use accent-color `#4C8DFF`. | L827-850, L2389-2423 |
| 6.5.5 | Create | Default name "<Type name> N", where N = count of that type + 1. The new id is `<type>a`, `<type>b`, … Visual types are placed centred at native size, scaled down to fit 1920x1080 (native sizes: display 1920x1080, window/device/spout/media 1280x720, image 400x400, browser 800x600, text 800x90, color 1920x1080, waveform 900x200, spectrum 900x240). Audio types get no transform. The node is inserted at the top of the parent (or of the tree), selected, and its Properties open. | L2432-2454 |
| 6.5.6 | Add existing | Adds a **reference** at the top of the parent (or tree), offset +40/+40, with the toast "Added a reference. Only its position, size, rotation and crop are its own." Nesting a scene inserts the scene node. | L2411, L2456-2460, L1307 |
| 6.5.7 | Double-click type | Creates a new source of that type at once (not for Scene). | L2386 |
| 6.5.8 | Blocking | Not available in a keyframe. | L2369-2372 |

### 6.6 Pop-out (projector) windows

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 6.6.1 | Window | Fixed, z-index 90, 420px wide, bg `#0A0B0E`, border `#3A3F48`, radius 8. Title bar 30px high, bg `#1F232A`, cursor move. Title "Scene: <name>" or "Source: <name>", plus a × close. The body is 16:9 on bg `#141A22`. | L571-586 |
| 6.6.2 | Placement and drag | The n-th window opens at (120 + n·32, 120 + n·32). The title bar drags it with pointer capture, clamped at ≥ 0. | L1924-1928, L1985-1997 |
| 6.6.3 | Content (simulated) | A source window shows one full-bleed coloured block with the source name (display `#243447`, webcam `#3A2F45`, lower `#0F1115`, alerts `#E8A33D`, logo `#4C8DFF`, ticker `#16191E`). The current scene shows coloured blocks at each item's placement and rotation. Any other scene shows "Live output of this scene". | L1964-1984 |
| 6.6.4 | Full-screen projector (simulated) | Choosing a monitor shows the toast: "Fullscreen projector: “<what>” on <monitor>. Press Esc on that monitor to close it." | L1930-1933 |

### 6.7 Toast

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 6.7.1 | Toast | role=status, fixed, bottom 24px, centred, max-width 520, padding 10px 14px, bg `#1F232A`, border `#3A3F48`, radius 8, 13px, z-index 150. It auto-hides after **4500 ms**, and a new toast replaces the current one. | L587-589, L1935-1939 |
| 6.7.2 | Toast texts | Reference added: "Added a reference. Only its position, size, rotation and crop are its own." · Clone added: "Added a clone. Changes to it don't affect the original." · "<name> is now a clone. Changes stay with it." · "<orig> reference restored. Local changes were discarded." · Demo ref: "<name> is now a clone. Changes stay in this scene." / "<name> is a reference again and follows the original in “<scene>”." · Group: "Created the scene “Group N”. It is nested here and also listed in Scenes." · Keyframe lock: "This is a keyframe of “<parent>”: only placement and visibility can change here. Edit everything else in “<parent>”." · Projector (6.6.4). | L1307, L1318, L1324, L3048, L2485, L2568, L1932 |

---

<a id="keyboard-shortcuts"></a>
## 7. Keyboard shortcuts (complete)
<!-- @section
-->

| # | Key | Scope | Effect | Lines |
|---|---|---|---|---|
| 7.1 | Pause | window | Toggle privacy screen | L1572 |
| 7.2 | F1-F5 (any `F<n>`) | window | Switch to the scene whose hotkey prop (saved, or PROP_DEFAULTS) equals the key: F1 Starting Soon, F2 Desktop + Cam, F3 Just Chatting, F4 BRB, F5 Ending. The browser default is prevented only on a match. | L1571-1580 |
| 7.3 | C | canvas focused | Toggle crop mode | L1755 |
| 7.4 | V | canvas | Move tool (crop off) | L1756 |
| 7.5 | Esc | canvas | Deselect (to scene), crop off, close context menu | L1757 |
| 7.6 | Ctrl/Cmd+A | canvas | Select all drawn items | L1758 |
| 7.7 | Arrows / Shift+Arrows | canvas | Nudge 1px / 10px | L1760-1766 |
| 7.8 | Shift (drag) | canvas | Half-speed pointer; add to or remove from selection on click; additive marquee | L1401, L1661, L1735 |
| 7.9 | Ctrl/Cmd (drag) | canvas | Move: no snap. Resize: stretch (free aspect). Rotate: 45° steps. Group scale: stretch. | L1407-1409, L1466, L1483, L1502 |
| 7.10 | Alt (drag handle) | canvas | Crop instead of resize | L1681 |
| 7.11 | Esc / Enter | any modal dialog | Close or cancel / press the primary button (not from a textarea, button or select) | L2183-2189 |
| 7.12 | ↑ ↓ Home End → ← Esc Tab | context menu / submenu | Navigation (see 5.1.6) | L3100-3122 |
| 7.13 | Hint-only (not implemented) | — | Ctrl+D, Ctrl+Shift+D, Del, F2, Ctrl+V, "Double-click" | L2919-2921 etc. |

---

<a id="scenes-keyframes-transitions"></a>
## 8. Scenes, keyframes, transitions (engine)
<!-- @section
-->

### 8.1 Scene data

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 8.1.1 | Start scene | "Desktop + Cam". The live `xf` and `trees.objects` hold the current scene. Other scenes are kept in `sceneData`. | L1212, L2519-2525 |
| 8.1.2 | Scenes tree | Folder **Show**: Starting Soon; Desktop + Cam (child keyframe "Cam focus"); Just Chatting. Folder **Overlays**: Intro overlay; Lower third pack; Cam shape. Then BRB and Ending at the root. | L947-952 |
| 8.1.3 | Objects tree (Desktop + Cam), top to bottom | alerts, lower, webcam, nested scene "Intro overlay" (logo, ticker), display. | L953-957 |
| 8.1.4 | Desktop + Cam placements (px) | display 77,65 1267×713. webcam 1384,497 460×410, crop l/r 24. lower 77,958 880×64. alerts 1600,60 280×120. logo 1720,250 160×160. ticker 0,1040 1920×40. | L908-915 |
| 8.1.5 | Other scenes | Just Chatting: webcam 300,70 1320×742; lower 300,850; alerts; logo 40,40 120×120; ticker. Starting Soon: logo 760,260 400×400 with ry 30, rx −6, persp 900; lower 520,740; alerts; ticker. BRB: logo 810,300 300×300; webcam 1500,760 360×202 with rot −4, ry −28, rx 8; lower 520,700; ticker. Ending: logo 860,340 200×200; lower 520,600. Intro overlay: logo, ticker. Each lists its own object order. | L919-946 |
| 8.1.6 | Unknown scene data | A nested scene or group with no saved data is built from its children in the current objects tree. | L2538-2545 |
| 8.1.7 | Switching | Clicking a scene in the Scenes panel switches to it (via `pick` with kind 'scenes'). So do the F-keys, "Switch to it" and the Transitions panel. Switching to the current scene only selects it. A switch saves the current scene (a keyframe saves only its diffs), loads the target, carries the shared opacity and blend from sources, resets the selection to the scene, turns crop off and closes the context menu. | L2678-2714, L3409-3413 |

### 8.2 Keyframes

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 8.2.1 | Model | A keyframe is a scene variant `{parent, label}` that stores only placement and visibility overrides (`x y w h rot rx ry z persp cl cr ct cb hidden`). Demo: "Desktop + Cam · Cam focus" (display 60,600 700×394; webcam 820,80 1000×891; lower hidden). | L921-925, L1109, L1215, L2547-2562 |
| 8.2.2 | Lock | Inside a keyframe these are blocked, with the lock toast: duplicate, reordering objects in the tree, Properties for objects, filter edit/add/set on objects, Add source, New group. Opacity and blend changes are not propagated to the source. Transform edits are allowed. The canvas banner (3.20) is shown. | L2564-2570, L1247-1255 |
| 8.2.3 | New keyframe | Named "<parent> · Keyframe N" with label "Keyframe N" (N = existing count + 1). Appended under the parent in the Scenes tree, then the app switches to it. Called on a keyframe, it creates a sibling under the same parent. | L2572-2586 |
| 8.2.4 | Reset to parent | Current keyframe: copies the parent's placements into the live canvas. Otherwise: clears its overrides. | L2588-2594 |
| 8.2.5 | Delete keyframe | If it is current, switches to the parent first, then removes the node and the keyframe record. | L2596-2606 |
| 8.2.6 | Tree rules | Keyframes cannot be dragged elsewhere in the Scenes tree. Only folders (`f:`) accept "into" drops in Scenes. A node cannot move into its own subtree. | L1795-1814 |

### 8.3 Transitions

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 8.3.1 | Presets | p1 "Smooth move" (Move, 600 ms, Ease in-out, unmatched Fade, stagger 0) is the **default**. p2 "Quick fade" (Fade 300). p3 "Slow fade" (Fade 900). p4 "Cut" (Cut 0, Linear). | L1217-1224 |
| 8.3.2 | Overrides | `into`: Ending→Slow fade, Lower third pack→Quick fade. `outOf`: BRB→Quick fade. Resolution order: **the destination's "into", then the source's "out of", then the default**. It reports `why` as into, out or default. | L1225-1229, L2608-2617 |
| 8.3.3 | Cut | Kind Cut, or duration ≤ 0, applies instantly. | L2697 |
| 8.3.4 | Move pairing | For a Move preset, each destination item is paired with an on-screen item that has the same **source id** (references resolve to their source), unless either side's per-item `match` is "Never". Unpaired destination items enter. Unpaired on-screen items leave. Fade pairs nothing, so everything cross-fades. Pairing works mid-transition from the frame currently on screen. | L2698-2713, L2673-2676 |
| 8.3.5 | Interpolation | Paired items: x, y, w, h, cl, cr, ct, cb and z are lerped. persp is lerped (default 1200). rot, rx and ry take the shortest-path angle, rounded to 0.1. Alpha goes from its from-alpha to 1. | L2760-2776 |
| 8.3.6 | Enter/leave effects | Per item `in`/`out`: Inherit (= preset `unmatched`), Fade (alpha only), **Scale** (shrinks to 65% about the centre, `1 − 0.35k`), **Slide** (toward the nearest canvas edge), or **None** (no animation: instantly gone or present). Leaving items fade alpha out alongside the effect. | L2737-2766 |
| 8.3.7 | Easing | Linear; "Ease out" (cubic out); "Spring" (`1 − e^(−6p)·cos(11p)`); default/"Ease in-out" (cubic in-out). A per-item easing of Inherit uses the preset's. | L2663-2670, L2737 |
| 8.3.8 | Timing | Each item's progress = `(now − t0 − index·stagger − delay)/duration`. The total length is duration + the max of (index·stagger + delay). It is driven by requestAnimationFrame and the anim is cleared at the end. Demo per-item setting: `Just Chatting\|lower` delay 150 ms. Per-item defaults: delay 0, easing Inherit, match Auto, in Inherit, out Inherit. | L1213, L2716-2729, L2781-2783 |
| 8.3.9 | Transitions panel API (passed as `trans`) | Exposes presets, defaultId, into/outOf, the scene list, current and sel. inOf/outOfScene. setOverride. usage(id) gives into/out counts. get/set by path `p.<id>.<key>`. setPreset. **addPreset** gives "New preset" (Fade, 500 ms, Ease in-out, unmatched Fade, stagger 0). **removePreset** refuses the default and removes overrides that used it. setDefault. resolve(from,to). sceneHas(scene,id). item(id) / setItem(id,k,v) for the current scene. selectScene. go (switch). | L2791-2835 |

---

<a id="sources-duplicates-and-groups"></a>
## 9. Sources, duplicates and groups (state the shell owns)
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| 9.1 | Demo types | display=Display capture, webcam=Video capture device, lower and ticker=Text, alerts=Browser, logo=Image. | L990 |
| 9.2 | Reference vs clone | `dups[id] = {of, mode}`. A **reference** shares everything with its source except placement (position, size, rotation, crop). Opacity and blend (`SHARED_XF`) are written to the source. A **clone** copies names, props, filters (new ids), favourite, labels and shared xf, and is independent from then on. New duplicates get id `<base>dupa`, `dupb`, …, are offset +40/+40, are inserted just above the original (or at a given parent), and are selected. Clone names get " copy". | L1100, L1260-1308 |
| 9.3 | Switch dup mode | Reference→clone copies the shared data in. Clone→reference drops its own name, props, filters, favourite and labels. Toasts as in 6.7.2. | L1310-1326 |
| 9.4 | Demo cross-scene refs | `obj:webcam` references Just Chatting's webcam and `scene:Intro overlay` is a reference. The menu "SOURCE" block toggles `clones[key]`. **Simulated: it only sets a flag and shows a toast.** | L1097, L3043-3051 |
| 9.5 | New group | Creates the scene "Group N" (first unused). With a selection, the selected objects (top first) move into it at the position of the first one. Without, it goes at the top. It is also appended to the Scenes tree. Toast. | L2462-2486 |
| 9.6 | Tree add | Scenes: "New folder" `f:newN`, or "New scene N" via treeAddScene (prepended and selected). Objects: New group (9.5). | L1816-1829 |
| 9.7 | Audio flags | `hasAudio`: scenes always; sources by type. `audioOn`: the saved `audio` prop, else the schema default (device off, others on). The menu "Audio" check toggles it. | L2488-2504 |
| 9.8 | Mixer channel list | Always "Mic · ES-9 Line" (−3 dB) and "Desktop audio" (−8.5 dB), then each distinct source with audio on, in objects-tree order, at −12 dB. **Simulated levels.** | L2505-2517 |
| 9.9 | Master/outputs values | Simulated meters: LUFS "−14.2" with the loudness filter on, otherwise "−17.8". Short-term 71 or 58. True peak "−1.1" with the true-peak limiter, otherwise "+0.4" with `tpOver`. fx count of active program filters. `monitorPost` toggle (default true). openFx shows the Filters panel for `out:program`. Outputs: YouTube stream (ready; video+audio), Recording (off; video+audio), Virtual camera (off; video only), each with an fx count and an open action. | L1103-1107, L2243-2259 |
| 9.10 | Labels and favourites (meta) | Labels: "Intro / outro" (flag, #E8A33D), "Gameplay" (gamepad, #4C8DFF), "Overlay" (layers, #B07CFF), "Sponsor" (sparkle, #E86BA6). Assignments: Starting Soon and Ending → Intro/outro; Desktop + Cam → Gameplay; Intro overlay, Lower third pack, alerts and lower → Overlay; display → Gameplay; ticker → Sponsor. Favourites: Desktop + Cam, Just Chatting, webcam. These apply to the source key (references share them). | L1189-1202, L2861-2873 |
| 9.11 | Transform actions (passed as `tx`, also used by menus) | Align left / horizontal centers / right / top / vertical centers / bottom, to the selection bbox (when 2 or more are selected and alignTo='selection') or to the canvas. Distribute horizontally/vertically (3 or more; equal gaps by centre order). Center on canvas (the selection as a block). Fit to canvas (contain, centred, rot 0). Stretch to canvas (0,0,1920,1080, rot 0). Match width/height (to the primary; 2 or more). Rotate 90° left/right. Reset rotation (rot, rx, ry = 0). Clear crop (restores the full extent). Disabled buttons have opacity 0.35. Summary: "Nothing selected. Click or Shift-click items on the canvas." / "1 item selected" / "N items selected". | L3126-3188 |
| 9.12 | Props passed to every Panel | vertical, kind, sel, picker, meta, metaset, xf, xfset, xfpatch, selids, tx, trees, treemove, treeadd, openctx, stream, filters, filtset, showpanel, names, openprops, mixorient, setmixorient, master, addfilter, editfilter, fltnew, keyframes, scenename, trans, srcinfo, mixer, clones, dups, srcof, baseof. | L139 |

---

<a id="simulated-or-non-functional"></a>
## 10. Simulated or non-functional (summary)
<!-- @section
-->

- File and Edit menu buttons, Studio mode and Record have no behaviour (L50-51, L82-83).
- The header stats "60 fps / 0 dropped / CPU 3%" are static (L79).
- Go Live / End stream only toggles a flag. Create stream adds a local entry. "Open in YouTube Studio" is always disabled (L107, L1877).
- Privacy blurs the canvas with CSS. There is no real mic mute (L3440).
- Pop-out windows show coloured blocks. The full-screen projector is a toast (L1930-1984).
- Mixer levels and master meters are fake numbers (L2250, L2507).
- Demo source visuals are drawn with CSS (L264-327).
- Disabled menu items: Paste, Paste scene, Rename, Delete folder/Ungroup, Show in preview, Duplicate scene, Delete scene, Open nested scene, Remove from scene. The shortcut hints Ctrl+D, Ctrl+Shift+D, Del, F2 and Ctrl+V have no handlers.
- Layouts are not persisted, and nothing is saved across reloads.
- Dead code: source tags (`look.tagged`), `cv.hint` and the snap text/border values, `closeActive` / tab `close`, `stopPd`, `PROP_DEFAULTS['scene:Desktop + Cam'].transition`.
