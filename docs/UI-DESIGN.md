<!-- @doc
id = "UI-DESIGN"
title = "UI design language"
kind = "normative"
status = "active"
review_every = "90d"
-->

# UI design language

One look for the web UI's menus, dialogs and secondary panels, so that each new screen
follows these rules instead of making its own. The main window's layout, the header bar,
the canvas and the Scenes and Sources panels keep their own prototype styling, but for
the shared menu style of their right-click menus ([spec-workspace](../design/ui-prototype/spec-workspace.md),
[spec-panels](../design/ui-prototype/spec-panels.md)). This page governs everything
those panels open, and every other panel. Where OBS has an answer that fits, follow it.
Where JStream works differently, or has a better answer, these rules win.

<a id="colour"></a>
## Colour roles
<!-- @section
covers = ["crates/jstream-ui/style.css"]
-->

The palette is the prototype's: near-black surfaces and one blue accent. Each colour has
one job, and a screen uses only these tokens from `style.css`.

| Role | Token | Use |
|---|---|---|
| Accent | `--accent` | Primary buttons, focus rings, checked controls, the selected tab's underline |
| Selected | `--selected` | A selected row, list item or segment |
| Button | `--button`, `--button-hover` | A secondary button's flat fill and its hover |
| Hover | `--menu-hover` | A hovered menu line or row: neutral, so it never looks selected |
| Danger | `--danger-text`, `--danger-solid` | Red text on a secondary button or menu line that deletes, and the solid button that confirms a delete |
| On air | `--danger` | Go Live, the program tag, live badges and live outputs only |
| Warning | `--warn` | Dropped frames and degraded health |
| Success | `--ok` | Healthy and connected |
| Text | `--text`, `--muted`, `--dim`, `--faint` | Body text, labels, help and section headers, disabled |

Don't use the on-air red for errors or warnings that aren't live.

The one exception is a series: the Sequence panel's lanes take `--accent`, `--ok`,
`--warn` and `--code-keyword` in turn (`.seq-lane.lane-N`), only to tell the lanes
apart.

<a id="buttons"></a>
## Buttons
<!-- @section
covers = ["crates/jstream-ui/style.css"]
-->

- **Primary** (`button.primary`): solid accent, white text. At most one per view, and
  the rightmost button of a dialog's footer.
- **Secondary** (a plain `button`, or `.secondary`): a flat fill with no border, so it
  never reads as a text field.
- **Danger** (`.danger.secondary`): a secondary button with red text, for an action that
  deletes. **Danger confirm** (`.danger`): solid red, only on the button that confirms a
  delete.
- **Ghost and link**: no fill, for light actions such as "Clear" or "+ Add filter". In a
  dialog, `button.link` is link-coloured text beside a field ("Edit…", "New…"), shown
  only when it can act rather than drawn disabled.
- **Small** (`button.sm`): 24 px tall with 12 px text, for a row of actions inside a
  panel (the Arrange panel's Fit, Match, Rotate and Crop rows, Set key). Its label can be
  short when a heading above says the rest ("Width" under Match); its tooltip and
  accessible name stay whole ("Match width").
- A dialog that only informs (About, Settings) ends with a secondary "Close", not a
  primary one.
- Focus is a 2 px accent ring outside the button.

<a id="menus"></a>
## Menus
<!-- @section
covers = ["crates/jstream-ui/style.css", "crates/jstream-ui/src/menu.rs"]
-->

The header's menus and the right-click menus are one family:

- 28 px lines. A 20 px leading slot is always kept, so labels line up in every menu.
  The slot holds a check for a shown panel, a checked option or the current layout.
  Toggle state is never shown as words.
- The trailing slot holds a shortcut in sans, or a submenu's current value followed by
  a `›`.
- Section heads are the [eyebrow](#type) style.
- An item that deletes is marked with `Entry::danger`, reads red, and sits in the last
  group. The Scenes and Sources panels' right-click menus share this menu style, but
  their delete items aren't marked.
- Menus hold commands. Information belongs in dialogs and status UI: the File menu's
  connection details are a dialog of their own, and the open project's name is the
  menu's head.
- A long menu is in groups, as OBS's source menu is: open it (Properties, filters);
  its state (Visible, Lock, Labels); the clipboard (Copy, Paste, Duplicate); its
  arrangement (an Order submenu for the layer order, Transform, Blending, Crop);
  other views (Pop out, Screenshot); then removal. A command that adds something new
  (Add source, New group) is in the menu of an empty spot, not an item's, with two
  exceptions that act on the item itself: a nested scene's Add source… adds into that
  scene, and the menu for several items has Group selection.
- A submenu opener is off when every line in it is, rather than open on lines that are
  all off. A command this build can't run is off with a tooltip that says where it
  works (Check for updates, in a browser: "Available in the desktop app"), rather than
  opening a dialog that says so.

<a id="type"></a>
## Type
<!-- @section
covers = ["crates/jstream-ui/style.css"]
-->

- IBM Plex Sans throughout ([ADR-0077](adr/0077-bundle-ibm-plex-fonts.md)): 15 px
  semibold for dialog and page titles, 13 px for body text and controls, 12 px for
  labels and help, 11 px for eyebrows.
- **Eyebrow**: every section header inside a dialog, a panel body or a menu is 11 px,
  semibold, upper case, tracked 0.06 em, in `--dim`. A count or a note beside it is
  sentence case.
- **Monospace** only for code, file paths and URLs shown read-only, codes a person
  types (pairing), and counters that tick. Shortcuts, summaries and log details are sans.
- Numbers that change in place (stats, gains, counters) use tabular figures.

<a id="controls"></a>
## Controls and lists
<!-- @section
covers = ["crates/jstream-ui/style.css"]
-->

- Selects, checkboxes and radios are drawn by `style.css`, never by the platform:
  a select with its own chevron, a 14 px checkbox with a 4 px radius that fills with
  the accent when checked.
- Toggles (`.switch`) are for settings that apply at once. Checkboxes are for options
  that a dialog commits with OK. A toggle outside a form is a toggle row
  (`.toggle-row`): its label, then the switch on the right.
- A hovered row is neutral (`--menu-hover`), and a selected row is `--selected`. The two
  never look the same.
- A row's summary or meta text gives way before its name: the name truncates last.

<a id="tabs"></a>
## Tabs
<!-- @section
covers = ["crates/jstream-ui/style.css"]
-->

Two levels, and never two rows of underlined tabs one under the other:

- **A panel strip** (the dock's tabs): uppercase, with the accent underline on the
  panel shown.
- **Inside a panel or dialog**: a segmented control (`.segmented`): a 2 px inset track
  with a border, 24 px segments in sentence case, the chosen one filled with
  `--selected`. `.full` spreads it over its row, as the Inspector's Transform, Motion and
  Behaviour do; a segment may end with a count ("Source 3", the Properties dialogs'
  "Filters 2"). The same control picks
  between a few values, as Align to's Selection and Canvas.
- **A dialog's pages** (Settings): a rail on the left of 32 px items in sentence case,
  the page shown filled with `--hover`, with no border.
- Two things don't share a name across the levels: the panel the prototype calls
  Transform is Arrange, so Transform is only the Inspector's tab.

<a id="dialogs"></a>
## Dialogs
<!-- @section
covers = ["crates/jstream-ui/style.css", "crates/jstream-ui/src/dialog.rs"]
-->

- Three widths: small (`--dialog-s`, 440 px) for a question or a short form, medium
  (`--dialog-m`, 600 px) for a one-column form, and large (`--dialog-l`, 860 px) for two
  panes. A two-pane dialog has a fixed height, so its panes don't jump.
- **Header**, 56 px: an optional icon, the title, and a subtitle that names the object
  or target ("Into scene “Main”", "output-1 · Stream · Stopped"). The title names the
  dialog in sentence case ("Output settings"), never with a raw ID first. A tagline is
  not a subtitle. The × is a 28 px target.
- **Body**: what scrolls fades out above the footer, so content never ends cut off
  under it.
- **Footer**, 56 px with 32 px buttons, one per dialog: Defaults, Delete… and similar on
  the left (`.footer-left`), Cancel and the primary on the right.
- **Commit model.** A setting that applies at once leaves a Close footer, as the
  Filters window's does, with a note on its left that Ctrl+Z undoes. One that is
  heavy to change waits in a draft for Save, with Cancel beside it; Save closes the
  dialog, and no dialog has Apply. In Settings only the Video page is a draft: the
  footer reads Cancel and Save while it is shown or holds a change, and Close otherwise.
- **A picker** (Add source, Add filter) is one layout: a 320 px list on the left, its
  search on top (it takes the focus) and its groups under eyebrows, an icon on each row;
  on the right the pick's 40 px icon tile (`.picker-tile`), name and one line, then
  what the pick needs, and where it lands as a callout. A double-click on a row adds it.
- **A code to read out** (Pair a device): 28 px mono, letter-spaced, on an inset card
  (`--app` with a `--line` border) with the time it has left under it, then the steps
  as a numbered list.

<a id="forms"></a>
## Forms
<!-- @section
covers = ["crates/jstream-ui/style.css", "crates/jstream-ui/src/panels/properties.rs"]
-->

The property grid, as OBS's Properties lay a source's settings out:

- In a dialog, labels sit in a 120 px column on the left, top-aligned with the first line
  of their control, in 12 px `--muted`.
- Controls are at most 360 px wide. A number field is 88 px with its unit beside it; a
  slider fills the row with a 64 px number after it.
- A setting's reset (↺) sits in a 20 px column of its own and shows only once the value
  differs from its default, so the right edges line up.
- A setting a controller can drive (live, a number with both ends or a switch) has a
  learn button (`button.learn`, the MIDI icon) after its reset, in a third column that
  takes only its width: `--muted` and borderless at rest, `--accent` with a border when
  bound (with its control's short name, `CC 7`) or while it is learning. A click opens
  its one menu (learn, make or follow a channel, remove). A setting that follows a
  channel shows the channel's name as a `--selected` chip in `--link` instead.
- The settings forms built from a kind's descriptor (`.field.prop`) get this layout by
  themselves; any other dialog form uses `.prop-grid` with `.prop-row`s.
- Booleans: a toggle (`input.toggle`) for a setting that applies at once, a checkbox for
  one that waits for OK. Both sit in the control column. A toggle's label is a `<label for>`, so a click on it
  flips the toggle.
- A file reads its path, then Browse… where the app can open a file dialog, then a
  short select of where the path starts.
- Related rows are grouped under [eyebrow](#type) headings.
- A panel's rows follow the same rule in less room: the Inspector's labels sit in an
  88 px column, its controls 28 px tall.
- A number field has no − and + steppers: the number and its unit share one box, and a
  drag across it, the arrow keys or typing change it. In a row of fields under one label
  (Position's X and Y), each field's short caption sits inside its box before the
  number, and a drag on the caption scrubs too. A pair leaves a 20 px gap between its
  fields, where Size's link sits, so every pair lines up. Only the mixer's gain fields
  keep steppers, for a single step without the keyboard.

<a id="copy"></a>
## Copy
<!-- @section
covers = ["crates/jstream-ui/style.css", "crates/jstream-ui/src/dialog.rs"]
-->

- A setting has at most one helper line: 12 px `--dim`, under its control, saying what
  the person needs to decide. The rest goes behind an (i) beside the label or control
  (the `Info` component), which shows on hover or focus, opens on the side with room
  inside the body that would clip it, takes no room while hidden, and is the mark's
  accessible name.
- Say what it does for the person, not how the engine does it.
- A warning or a consequence that needs reading is a one-line callout (`.callout`, with
  the info icon; `.warn` when it stops something), not a paragraph.
- An empty list says so in a few words ("No motion yet.") next to the action that fills
  it.
- Everything in a chain is a filter: a source's, a scene's, an output's or one item's.
  An item's own are "item filters", never "effects".
