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
the canvas and the Scenes and Sources panels keep their own prototype styling
([spec-workspace](../design/ui-prototype/spec-workspace.md),
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
| Hover | `--menu-hover` | A hovered menu line or row: neutral, so it never looks selected |
| Danger | `--danger-text`, `--danger-solid` | Red text on a secondary button or menu line that deletes, and the solid button that confirms a delete |
| On air | `--danger` | Go Live, the program tag, live badges and live outputs only |
| Warning | `--warn` | Dropped frames and degraded health |
| Success | `--ok` | Healthy and connected |
| Text | `--text`, `--muted`, `--dim`, `--faint` | Body text, labels, help and section headers, disabled |

Don't use the on-air red for errors or warnings that aren't live.

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
- **Ghost and link**: no fill, for light actions such as "Clear" or "+ Add filter".
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
  group. The Scenes and Sources panels' menus are left as they are.
- Menus hold commands. Information belongs in dialogs and status UI.

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
  that a dialog commits with OK.
- A hovered row is neutral (`--menu-hover`), and a selected row is `--selected`. The two
  never look the same.
- A row's summary or meta text gives way before its name: the name truncates last.
