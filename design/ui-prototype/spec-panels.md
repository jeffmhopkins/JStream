<!-- @doc
id = "DESIGN-UI-SPEC-PANELS"
title = "UI prototype spec: panels"
kind = "reference"
status = "active"
review_every = "90d"
-->

# UI prototype spec: panels

What [`Panel.dc.html`](Panel.dc.html), the panel contents of the [UI prototype](README.md) (direction D), does, written as requirements. One component draws every panel; its `kind` prop picks which (L724, L1810). The workspace around the panels (title bars, docking, context menus, dialogs, the transition engine and the shared state the panels read through props) is in [the workspace spec](spec-workspace.md). [UI features](../../docs/UI-FEATURES.md) tracks which of these the real UI has.

Line references (`L123`) point into `Panel.dc.html` as committed. *Simulated* marks prototype behaviour that is faked; it is not a requirement.

<a id="global-look-and-shared-building-blocks"></a>
## 0. Global look and shared building blocks
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| G1 | Theme | Dark only (`color-scheme: dark`). Panel body: background `#16191E`, text `#E6E8EB`, 8px padding. Font IBM Plex Sans 13px (weights 400/500/600). Numbers and mono text use IBM Plex Mono (400/500), loaded from Google Fonts. | L11-13, L36 |
| G2 | Palette | Field background `#0F1115`. Border `#2E333B`. Raised row/tile `#1F232A`. Popover/inset `#1A1D23`. Stepper button `#1A1D23`, hover `#2B313A`. Secondary text `#9AA1AC`. Muted `#7D8590`. Faint hint `#5A606B`. Dimmed `#6B717C`/`#4D535E`. Accent blue `#4C8DFF`. Selected bg `#22324A` (selected hover `#26385A`). Link text `#8AB4FF`. Light blue `#B5CEFF`. Green `#3FB27F`/`#7FD6A8`. Amber `#E8A33D`. Red `#FF5C63`/`#FF8A8F`/`#C62F36`. Purple `#B07CFF`. Star gold `#F2C14E`. Selection highlight `#2B4A7A`. | L13-33 |
| G3 | Scrollbars | Thin. Thumb `#3A3F48` (hover `#4D535E`), 10px wide, radius 6, 2px border `#16191E`, transparent track. | L17-22 |
| G4 | Focus ring | `input/button:focus-visible` gets a 2px `#4C8DFF` outline, offset −1px. | L24 |
| G5 | Row hover | `.row:hover` bg `#1C2028`. Selected row hover `#26385A`. `.ib:hover` (icon button) bg `#2B313A`. `.sechd:hover` text turns `#E6E8EB`. | L26-33 |
| G6 | Hover-reveal controls | `.hov` elements have opacity 0, fading in over 120ms on row hover, on focus-visible, or when they carry class `on` (state kept on, e.g. a favourited star or a hidden-eye). | L29, L31 |
| G7 | Icons | 24×24 stroke icons, one path each, `stroke-width` 1.8 (2 for small chips), round caps and joins. The `ICONS` table (id → [name, path]) has: monitor Display, window Window, webcam Webcam, mic Microphone, speaker Audio output, globe Browser, image Image, text Text, film Video file, palette Color, music Music, layers Scene, keyframe Keyframe (diamond), spout Shared video, wave Waveform, bars Spectrum, folder Folder, play Starting, coffee Break, chat Talk, gamepad Gameplay, flag Intro / outro, clock Countdown, user Person, hash Topic, star Star, heart Heart, bolt Bolt, sparkle Sponsor, bell Alert, tag Tag, eye Visible, lock Locked, wrench Setup. Further constants: EYE_ON, EYE_OFF, OUT_ICON (broadcast arcs), MASK_ICON (square with a circle), FX_ICON (sparkle), LINK_ICON (chain). | L726-761, L881-886 |
| G8 | Icon groups (label icon picker) | Sources: monitor, window, webcam, spout, mic, speaker, globe, image, text, film, palette, music, wave, bars. Scenes: layers, folder, play, coffee, chat, gamepad, flag, clock, user, hash. General: star, heart, bolt, sparkle, bell, tag, eye, lock, wrench. | L762-766 |
| G9 | Colour swatches | `#4C8DFF, #2EC4D6, #3FB27F, #E8A33D, #E5484D, #E86BA6, #B07CFF, #9AA1AC`. | L830 |
| G10 | Number field (scrub field) | A bordered box (bg `#0F1115`, border `#2E333B`, radius 4, height 28-30, overflow hidden) holding: a **−** stepper button (22-26px wide, bg `#1A1D23`, right border); a right-aligned mono 12px text input with cursor `ew-resize`, `inputmode=decimal`, tooltip "Drag to change · Shift: fine · Ctrl: coarse · or type a value"; a unit label (11px `#7D8590`); a **+** stepper button. Behaviour: (a) −/+ step by `step`. (b) Typing edits a draft. Commit on blur or Enter (then blur). Escape cancels and blurs. The parser accepts the `−` (U+2212) minus and a `,` decimal. The value is clamped to [min,max] and rounded to `dec` places. (c) ArrowUp/Down step ×1. With Shift the step is ×0.1 (only when step<1, otherwise ×1). With Ctrl/Meta/Alt it is ×10. (d) Pointer scrub: only when the field is not focused. Movement starts after a 3px threshold, then pointer capture and blur. Each 3px of travel = 1 step. Shift = ×0.1 and Ctrl/Meta/Alt = ×4, applied incrementally so changing a modifier mid-drag never jumps. Accumulated value is clamped. `axis:'y'` fields scrub vertically (drag up increases). (e) Display uses `fmtNum`, which writes negatives with the U+2212 "−" sign. Default min/max is ±100000. Default dec is 1 if step<1, else 0. | L223-226, L1308-1378, L921-924 |
| G11 | Section header (foldable) | A button with a caret `▾`/`▸` (9px, 10px wide) and an UPPERCASE title (11px, weight 600, letter-spacing 0.8px, `#9AA1AC`). When folded it shows a hint after it (weight 400, `#5A606B`, prefixed " · "). Fold state is remembered per section title in `secClosed`. **Default folded: `3D` and `SCENE SWITCHES`.** | L217, L1704-1707, L936 |
| G12 | Popovers close | A pointerdown outside a `data-pop="<name>-<uid>"` element closes the Filter popover, the Tag (labels) popover, and `addOpen`. Escape inside a popover closes all of them (stopPropagation). | L942-953, L1813 |
| G13 | Checkbox look (popover rows) | A 14×14 box, radius 3, 1.5px border. Unchecked: border `#4D535E`, transparent. Checked: border and fill in the item colour, with a dark `✓` (10px, `#0F1115`). | L56, L160 |
| G14 | Label chip (on rows) | 18×18, radius 4, background = the label colour at 16% alpha. Holds a 12px icon stroked in the label colour. Tooltip is the label name. | L137-141 |

---

<a id="scenes-panel"></a>
## 1. Scenes panel (`kind: scenes`)
<!-- @section
-->
It shares the tree implementation with Scene objects (L38-177, L1381-1547).

| # | Feature | Requirement | Lines |
|---|---|---|---|
| S1 | Layout | A column with gap 6 that fills the panel height (`100cqh - 16px`). Order: search/filter bar → (optional) label manager → row list → empty state. | L39 |
| S2 | Search box | 28px high. Bg `#0F1115`, border `#2E333B`, radius 5. Magnifier icon 14px `#7D8590`. Placeholder **"Search scenes"** (aria same). Case-insensitive substring match on the row label. | L41-45, L1422, L1427 |
| S3 | Filter button | A 28×28 funnel icon button, tooltip "Filter by favorites and labels". When favourites-only or any label is active: bg `#22324A`, border `#4C8DFF`, icon `#B5CEFF`. Otherwise bg `#0F1115`, border `#2E333B`, icon `#9AA1AC`. **Count badge** at top-right (−6,−6), 16px pill, bg `#4C8DFF`, dark 10px/600 text. It shows while filtering. Count = favOnly(1) + number of labels + (search non-empty ? 1 : 0). | L46-52, L1828-1833 |
| S4 | Filter popover | 220px wide, at top 34 right 0. Bg `#1F232A`, border, radius 8, shadow `0 12px 32px rgba(0,0,0,.5)`. Rows: a checkbox, a gold filled star and **"Favorites only"**. Then the heading **"LABELS · ANY OF"**. Then one checkbox row per label (icon in the label colour and its name). OR semantics: an item matches if it has any checked label. Then a divider and a footer: **"Clear"** (`#9AA1AC`; clears search, favourites and labels) and **"Edit labels…"** (`#8AB4FF`; opens the label manager and closes popovers). | L53-74, L1658-1669, L1836, L1840 |
| S5 | Filtering semantics | Folders never match on their own. A match keeps its nearest shallower ancestor visible (a parent stays visible when a child matches). Kept non-matching ancestors are drawn **dimmed** (text `#6B717C`, icon `#4D535E`). **Drag-reorder is disabled while filtering.** | L1416-1441, L1450-1452, L1468-1469 |
| S6 | Empty state | "Nothing matches these filters." (padding 12×8, `#7D8590`). Shown when the tree has no rows. | L170-172, L1850 |
| S7 | Label manager (inline card) | Opened by "Edit labels…". A card with bg `#1A1D23`, border, radius 6, padding 10. Header **"LABELS"** with a **"Done"** link that closes it. Per label (sub-card bg `#16191E`): (a) a 28px **icon button** bordered in the label colour, tooltip "Change icon", which toggles the icon picker; (b) a **name input** (rename on change); (c) a **×** delete button, tooltip "Delete label". Deleting removes the label from every assignment and from the active filter. (d) A row of 8 **colour swatches** (18px circles; the current one has a 2px `#E6E8EB` ring). (e) The icon picker, when open: three groups (Sources / Scenes / General, title 11px `#7D8590`) in a 7-column grid of 30px icon buttons. The current icon gets the label colour as border and a 22% tint as bg. Footer: **"+ New label"**, which adds `{name:'New label', icon:'tag', color: SWATCHES[n % 8]}` and opens its icon picker. | L78-115, L1671-1693, L1842-1848 |
| S8 | Default labels | l1 "Intro / outro" flag `#E8A33D`; l2 "Gameplay" gamepad `#4C8DFF`; l3 "Overlay" layers `#B07CFF`; l4 "Sponsor" sparkle `#E86BA6`. Default assignments: Starting Soon→l1, Ending→l1, Desktop + Cam→l2, Intro overlay→l3, Lower third pack→l3, obj alerts→l3, obj lower→l3, obj display→l2, obj ticker→l4. Default favourites: Desktop + Cam, Just Chatting, obj webcam. Labels and favourites are keyed by the *source* key, so a reference shares them with its original. | L831-844, L1428-1429, L1444 |
| S9 | Default scene tree | Folder **Show** (Starting Soon [play], Desktop + Cam [layers], Just Chatting [chat]). Folder **Overlays** (Intro overlay [layers], Lower third pack [layers]). Then top-level **BRB** [coffee] and **Ending** [flag]. | L845-855, L909-914 |
| S10 | Row layout | 30px high, radius 5, gap 2. Left padding = `6 + depth*18` px. Contents in order: **caret** (16px slot, `▾`/`▸` 10px `#7D8590`, visibility hidden when not a container); a main button holding a [link-role badge] (objects only), the **type icon** (15px, `#9AA1AC`), the **label** (ellipsis), a **badge** (11px `#FF8A8F`), and a **shared** text (11px `#7D8590`); then **label chips**; then hover buttons **Labels** (tag icon) and **Favorite** (star). Folders and ref rows have no tag/star (`canMark`). | L124-155, L1525 |
| S11 | "program" badge | The scene that is currently on program (`scenename`, default "Desktop + Cam") shows the text badge **"program"** in `#FF8A8F` after its name. | L1470 |
| S12 | "shared ×N" text | A slot exists (`r.shared`), but the prototype **always sets it to ''**. No shared count renders in this file. If "shared ×3" is required, it comes from another file or is unimplemented here. | L135, L1471 |
| S13 | Folder rows | Use the folder icon. Label text is `#6B717C` (dim). Clicking a folder does not select it. Double-click does nothing. No tag/star. | L1394, L1401, L1469, L1476-1478 |
| S14 | Keyframe rows | A scene listed in `props.keyframes` uses the **keyframe** (diamond) icon in purple `#B07CFF`. It is **not draggable**. A scene's children in this tree are its keyframes. | L1397, L1401, L1468, L1486, L1501 |
| S15 | Selection | Click selects via `picker(key, {additive: shiftKey, kind})`. Shift-click = additive. Selected row bg `#22324A`. Rows whose id is in `selids` are also highlighted (objects). | L1474-1479, L1650-1653 |
| S16 | Double-click | Calls `openprops(key)`, which opens the item's Properties dialog. Not for folders. | L1478 |
| S17 | Right-click | Row: `openctx(kind, key, x, y)`. Empty panel area: `openctx(kind, null, x, y)`. The context-menu items are defined by the host, not here. | L39, L1508-1512, L1854-1858 |
| S18 | Collapse/expand | The caret toggles `collapsed[kind\|key]`. aria-label "Expand X"/"Collapse X", aria-expanded. Collapsed children are skipped. | L125, L1396, L1403, L1480-1485 |
| S19 | Drag to reorder / nest | HTML5 DnD (`application/x-jstream-tree`). Same tree only. **Drop indicator:** before = inset top 2px `#4C8DFF` line; after = inset bottom 2px line; into = inset 2px outline round the row. A container row takes before (<25%), into (25-75%) or after (>75%). Others split at 50%. **In Scenes only folders (`f:`) accept "into"** (a scene's children are keyframes). Drop calls `treemove(kind, dragKey, targetKey, mode)`. After an "into" drop the target expands. Drag is only enabled when the `treemove` prop exists and you are not filtering. Keyframe and ref rows are not draggable and ref rows reject drops. | L1450-1451, L1486-1524 |
| S20 | Label popover (per row) | Opens from the tag button, inline under the row (margin-left = indent). Bg `#1A1D23`, border, radius 6. Checkbox rows per label toggle the assignment. Footer **"Edit labels…"** (12px `#8AB4FF`). Opening it closes the Filter popover. The tag button stays visible (`on`) while open. | L156-167, L1532-1544 |
| S21 | Favorite star | A 14px star. On: fill and stroke `#F2C14E` and it stays visible. Off: hollow `#9AA1AC`, visible on hover only. Toggles `meta.favs[srcKey]`. | L151-153, L1526-1530 |

<a id="scene-objects-panel"></a>
## 2. Scene objects panel (`kind: objects`)
<!-- @section
-->
Same tree machinery as Scenes, with these differences.

| # | Feature | Requirement | Lines |
|---|---|---|---|
| O1 | Caption | A line above the list: **In “<sceneName>”, top to bottom** (12px `#7D8590`, padding 2×4). sceneName = `scenename` prop, default "Desktop + Cam". Order is top-most first. | L117-119, L1905 |
| O2 | Search placeholder | "Search objects". | L1811 |
| O3 | Default objects | Alerts [globe], Lower third [text], Webcam · Brio [webcam], **Intro overlay** [layers] (a nested scene acting as a group, children Logo [image] and Ticker [text]), Display 1 [monitor]. | L868-876, L915-919 |
| O4 | References in italics | An object that references a source owned by another scene draws its label in *italics* (defaults: obj:webcam and the nested scene Intro overlay), unless the host marks it as an independent clone (`clones[key]`), or `dups[id].mode === 'reference'`. Comment: "turned into an independent clone (right-click)". | L865-867, L1243-1247, L1472 |
| O5 | Visibility eye | Shown only for objects present in the `xf` transforms. Hover-revealed. Stays visible (class `on`) when hidden. Icon EYE_ON / EYE_OFF (slashed). Tooltip "Visible in this scene". aria "Show or hide <label>". Toggles `xf[id].hidden` via `xfset`. A hidden object's label is dimmed `#6B717C`. Visibility is per placement, and a keyframe can change it. | L142-146, L1455-1460, L1469 |
| O6 | Filter-reference child rows | When an object has a filter that uses another scene (Alpha mask, Luma matte, Clip to, Displacement) with a scene chosen, a **child row** appears under it (depth+1) labelled with that scene name. The parent becomes collapsible. The row starts with a **role badge**: an 18px square, radius 4, bg = role colour at 20% (`33` hex alpha), holding a 12px icon. Mask types use MASK_ICON purple `#B07CFF`. Displacement (`ref:'fx'`) uses FX_ICON amber `#E8A33D`. A disabled filter gives grey `#5A606B`. Tooltip: "<Role>[ (off)]: uses the scene “<scene>”". Clicking the row selects the scene via `picker('scene:'+scene)`. No tag/star, not draggable, rejects drops. Defaults produce Webcam → "Cam shape" (Alpha mask, off, grey) and Logo → "Lower third pack" (Displacement, amber). | L127-131, L1385-1390, L1405-1409, L1461-1464 |
| O7 | Nested-scene group drop | In Objects, any group (including a nested scene) accepts "into" drops. | L1502 |
| O8 | Multi-select highlight | Rows whose id is in `selids` get the selected bg. | L1473 |

<a id="inspector"></a>
## 3. Inspector (`kind: inspector`)
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| I1 | Header row | 30px, bg `#1F232A`, radius 5. Contents: type icon (15px `#9AA1AC`); **title** (600, ellipsis, italic if a reference); **kind tag** (11px `#7D8590`); **"+N"** more-selected count (11px `#B5CEFF`); assigned label chips; a **Labels** tag button (always visible here) opening the same label popover with "Edit labels…"; a **Favorite** star. Sections below are separated by a 14px gap. | L180-213, L1720-1734 |
| I2 | Kind tag values | Scene → "Scene". Keyframe → `Keyframe of “<parent>”` (diamond icon). Output → "Master chain" (program) or "Output". Object → the first segment of its subtitle with " source" removed (e.g. "Browser", "Text", "Video capture", "Image", "Display capture"). | L1737, L1742, L1746 |
| I3 | Object subtitles (data) | alerts "Browser source · only in this scene". lower "Text source · in 2 scenes". webcam "Video capture · used in 3 scenes". logo "Image · inside “Intro overlay”". ticker "Text · inside “Intro overlay”". display "Display capture · 3840 × 2160". With a multi-select it appends " · +N more selected". (The subtitle is computed but **not rendered**; only the kind tag and "+N" render.) | L887-900, L1746 |
| I4 | Output selection | `out:program` "Program output", `out:youtube` "YouTube stream", `out:record` "Recording", `out:vcam` "Virtual camera". The output icon is drawn, with no chips and no body sections (only the header and its tag/fav). | L1735-1737 |
| I5 | SCENE section (scenes) | Foldable "SCENE". Key/value rows: **Objects** = count, **Used as a source in** = list or "none", **Hotkey** (mono) = F1…F5 or "none". A `sceneFields` number-field slot exists but is always empty. Data: Starting Soon 3/none/F1; Desktop + Cam 5/none/F2; Just Chatting 4/none/F3; Intro overlay 2/"Desktop + Cam, BRB"/none; Lower third pack 3/"Just Chatting"/none; BRB 3/none/F4; Ending 2/none/F5. | L215-234, L856-864, L1698 |
| I6 | TRANSITIONS section (scenes) | Foldable "TRANSITIONS". Two rows: **Transition in** and **Transition out**, each a 150px select. Options: "Default (<default preset name>)" (value '') then every preset name. Writes `trans.setOverride(scene, 'into'/'outOf', id or null)`. Note: **"If both scenes override, the next scene's “in” wins."** (11px `#7D8590`). Folded hint: e.g. "in Fade, out Swipe" (only directions that are set). Shown for scenes only, and only if the `trans` prop exists. | L236-249, L1602-1613, L1870 |
| I7 | PARAMETERS section (objects) | Foldable. A 2-column grid of number fields: **X** px, **Y** px (min −10000), **W** px, **H** px (min 1), then **W %** and **H %** (the same size as a % of the source's native size, min 0.1, max 10000, default 100), then **Rotation** ° (step 0.5, 1 dec, ±360). Defaults per object `t` (e.g. webcam 1384, 497, 460×410). Native sizes: display 3840×2160, webcam 1920×1080, lower 880×64, alerts 560×240, logo 512×512, ticker 1920×40, else 1920×1080. Values are bound to the shared `xf` transforms, so the canvas and Inspector edit the same numbers. | L1747-1751, L878, L1259-1269, L1284 |
| I8 | W/H link toggle | A button in the PARAMETERS header, right-aligned: link icon + text **"W/H"**, 24px high, radius 5, 11px. On (default): bg `#22324A`, border `#4C8DFF`, text `#B5CEFF`, chain icon, tooltip "Width and height are linked (keeps the aspect ratio). Click to unlink." Off: transparent, border `#2E333B`, `#9AA1AC`, broken-chain icon, "Width and height are independent. Click to link them." When linked, editing W or H patches the other to keep the current aspect ratio (2-decimal rounding, min 1) via `xfpatch`. | L258-261, L1756-1760, L1299-1304 |
| I9 | 3D section (objects) | Folded by default. Fields: **Tilt (X)** ° ±180 step 0.5; **Turn (Y)** ° ±180 step 0.5; **Depth (Z)** px ±5000; **Perspective** px 100–20000 step 10, default 1200. Folded hint "in use" when any of the first three is non-zero. | L1761-1766, L1864 |
| I10 | CROP section (objects) | Fields **Left, Right, Top, Bottom** px, 0–4000. Defaults from data (webcam crop L24 R24). Folded hint "cropped" when any is non-zero. | L1752, L1767, L1864 |
| I11 | Audio-only notice | For audio-only sources: no placement sections. It shows "Audio only: no picture, so nothing to place. Its level is in the Mixer." Blending and switches are hidden. | L281-283, L1754-1755, L1859 |
| I12 | Keyframe lock notice | When the current scene is a keyframe and an object is selected: a purple callout (bg `rgba(176,124,255,.10)`, border `#4E3A7A`, text `#D9C6FF`, 12px, `◆` prefix): "Keyframe of “<parent>”: blending, filters and properties follow it. Only placement and visibility change here." Blending and Scene-switch sections are hidden, but placement fields remain. | L284-286, L1700-1702, L1859 |
| I13 | BLENDING section | Foldable "BLENDING". Hint = mode name when not Normal. 2-column grid: **Mode** select (Normal, Additive, Multiply, Screen, Overlay, Darken, Lighten, Difference, Exclusion), which writes `xf[id].blend`; **Opacity** % 0–100, default 100, bound to the *source's* xf. | L287-310, L879, L1771-1777, L1871 |
| I14 | SCENE SWITCHES section | Folded by default. Hint "customised" when any value differs from default. A **Delay** ms field (0–3000, step 10, 128px wide). Then 150px selects: **Easing** (Inherit, Ease in-out, Ease out, Linear, Spring); **Glide to other scenes** (Auto, Never); **Appear** (Inherit, Fade, Scale, Slide, None); **Disappear** (Inherit, Fade, Scale, Slide, None). Writes `trans.setItem(id, k, v)`. A per-scene preview list (`go`, e.g. "glides to its place" green / "cross-fades" / "leaves · fade" amber / "fades out" / "cuts") is computed but **not rendered** in this file. | L313-337, L1616-1643, L1868 |
| I15 | Generic list sections | A template for foldable lists of rows (each a label plus a 128px number field on a `#1F232A` row), "None" when empty, and an add link (`sec.add`). `listSections` is always `[]`, so it is not visible. | L341-360, L1778 |
| I16 | Dead/computed but unrendered | `labels` (label chips with Add/Remove), `fsTitle` ("SCENE FILTERS" / "SOURCE FILTERS · EVERYWHERE"), `fsum` filter summary, `editFilters` → `showpanel('filters')`, `linked`. These are leftovers of an earlier Inspector filter summary that is no longer drawn. | L1708-1719, L1124-1129, L1872-1876 |

<a id="transform-arrange-panel"></a>
## 4. Transform / Arrange panel (`kind: transform`)
<!-- @section
-->
The content is supplied by the host prop `tx`. Fallback summary: "Shown in the workspace with live selection."

| # | Feature | Requirement | Lines |
|---|---|---|---|
| T1 | Summary | A `tx.summary` text line (`#9AA1AC`) at the top. | L368 |
| T2 | Align-to toggle | "Align to" plus a segmented group: **Selection** / **Canvas**. Active segment bg from `tx.selBg/canvasBg`. Selection has an opacity (`selOp`, disabled look). | L369-375 |
| T3 | ALIGN | A 6-column grid of 32px icon buttons (17px icons, stroke 1.7, `#C9CED6`, bg `#0F1115`). They come from `tx.alignBtns` with label, tooltip, disabled and opacity. Expected 6 (left/centre/right/top/middle/bottom). | L376-385 |
| T4 | DISTRIBUTE · 3 OR MORE | The same grid. Expected 2 buttons (horizontal/vertical). Disabled below 3 selected. | L386-395 |
| T5 | ARRANGE | A 2-column grid of text buttons (bg `#1F232A`, 12px, left-aligned) from `tx.actions` (placeholder count 9). | L396-403 |

<a id="filters-panel"></a>
## 5. Filters panel (`kind: filters`)
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| F1 | Target | The selected `obj:`, `scene:` or `out:` key. A reference's filters resolve to its source (`srcof`). With no target: title "Nothing selected" and the text "Select a source or a scene." The Add button is hidden. | L977-981, L990, L544 |
| F2 | Header | An 18px icon (`#9AA1AC`), **title** 14px/600, and a **note** 11px `#9AA1AC`. Notes: source "Applies everywhere this source appears, top to bottom."; scene "Scene filters apply to the whole scene, top to bottom."; program "Master chain: applies to everything leaving JStream."; youtube/record "Only this output, after the program filters."; vcam "Only this output, after the program filters. No sound." | L474-480, L989, L1100 |
| F3 | Chains | Separate **VIDEO** (monitor icon) and **AUDIO** (speaker icon) sections. The header shows "N filter(s)" on the right (11px `#7D8590`). Which chains appear: scenes → video only; sources → video unless audio-only, audio if the source has audio; outputs program/youtube/record → both; vcam → video only. | L481-487, L992-995, L1088-1101 |
| F4 | Empty chain | A dashed-border box (`#2E333B`, radius 6, centred 12px `#7D8590`) with "No video filters." / "No audio filters." | L536-538, L1095 |
| F5 | Filter row | 30px, bg `#1F232A`, radius 5, `cursor: grab`, tooltip "Double-click for its settings · drag to reorder". In order: an **eye toggle** (24px; on = EYE_ON `#C9CED6`, off = eye-off `#5A606B`; aria "Turn off/on <name>"); the **name** (on `#E6E8EB`, off `#7D8590` with line-through); a **summary** (max 55% width, 12px); a **caret** (▸/▾ for easy filters, `⤢` for dialog filters, none for filters with no params); a hover **×** Remove. | L490-500, L1038-1058 |
| F6 | Summary text | Ref filters show the scene name in *italic* `#B5CEFF`, or "no scene" in amber `#E8A33D`. One numeric param → its value + unit (mono `#9AA1AC`, e.g. "0.20", "−6.0 dB"). Several params → up to 2 changed ones as "Label value · Label value". None changed → "defaults". No params → ''. | L1031-1039 |
| F7 | Easy vs dialog | **Easy filter = 1-3 params.** Clicking it expands it in place (tooltip "Click to adjust here · double-click for the full dialog"). Other filters open the host dialog via `editfilter(target, id)` (tooltip "Click to open its settings"). Double-click always opens the dialog. A filter just added from the dialog (`fltnew`) opens expanded automatically. | L772, L1017, L1042-1043, L1057 |
| F8 | Expanded editor | Under the row: bg `#1A1D23`, border with no top, bottom radius 6, a 2-column grid. Param types: **num** (scrub field 28px); **select**; **scene** (full-width select, first option "Choose a scene…", then scenes excluding the current scene, any scene containing this source, and keyframes of those); **text** (full-width mono input, e.g. a file path); **color** (full-width swatch row: default + 8 swatches + `#FFFFFF` + `#0F1115`, deduped, 20px circles with a selected ring `#E6E8EB`). | L501-533, L1021-1029, L996-1010 |
| F9 | Drag reorder | Within the same chain only (video↔audio refused). MIME `application/x-jstream-filter`. Drop line inset 2px `#4C8DFF` top/bottom by half. | L1060-1085 |
| F10 | Add button | **"+ Add filter…"** (bg `#1F232A`, border, radius 6, padding 6×12) → `addfilter(target)`, which opens the host's add dialog. | L541-543, L1104 |
| F11 | Filter catalogue (video) | Each entry: group, name, hint, params (default, min–max, step, unit). **Color and keying:** Color correction "Gamma, contrast, saturation, hue" (Gamma 0, −3..3, 0.05, 2 dec; Brightness 0 ±100; Contrast 0 ±100; Saturation 0 ±100; Hue shift 0 ±180 °; Color multiply #FFFFFF). Apply LUT "A .cube or .png colour look" (LUT file `C:\LUTs\warm-film.cube`; Amount 100 0-100 %). Chroma key "Remove a green or blue screen" (Key color Green/Blue/Magenta/Custom; Similarity 40 %; Smoothness 8 %; Spill reduction 10 %). Color key "Remove any one colour" (Color #3FB27F; Similarity 30 %; Smoothness 5 %). Luma key "Remove dark or bright areas" (Luma min 0 %; Luma max 100 %; Smoothness 4 %). **Detail:** Sharpen "Crisper detail" (Amount 0.2, 0..1, 0.05, 2 dec). Blur "Gaussian, whole image or an area" (Size 8 0-200 px; Area Whole image/Rectangle; Area X 0 0-1920 px; Area Y 0 0-1080 px). **Style:** Rounded corners (Radius 16 0-500 px; Border 0 0-50 px; Border color #FFF). Drop shadow "Soft shadow behind" (Offset X 6 ±200 px; Offset Y 8; Blur 16 0-200; Opacity 40 %; Color #0F1115). Outline (Width 3 0-50 px; Color #FFF). Glow (Size 20 0-200 px; Strength 60 %; Color #4C8DFF). **Motion and timing:** Scroll (Speed X 80 ±2000 px/s; Speed Y 0). Video delay (Delay 0 0-5000 ms step 10). **Uses another scene:** Alpha mask "Cut out by another scene’s shape" (Mask scene; Invert No/Yes) ref mask. Luma matte "Bright areas of a scene show this" (Matte scene; Invert) mask. Clip to "Only show inside another scene" (Clip scene) mask. Displacement "Warp by another scene’s brightness" (Map scene; Strength 20 0-200 px) ref fx. **Advanced:** Custom shader "Your own WGSL effect, time-limited" (Shader file `effects\crt.wgsl`; Strength 50 %). | L778-797 |
| F12 | Filter catalogue (audio) | **Levels and dynamics:** Gain "Turn it up or down" (Gain 0 ±30 dB step 0.5). Compressor "Even out loud and quiet" (Ratio 4 1-32 :1 step 0.5; Threshold −18 −60..0 dB; Attack 6 1-500 ms; Release 60 1-1000 ms; Output gain 0 ±32 dB; Duck under Nothing / Mic · ES-9 Line / Desktop audio). Limiter "Never go over a level" (Threshold −6 −60..0 step 0.5 dB; Release 60 ms). Noise gate "Silence below a level" (Open threshold −26 −96..0 dB; Close threshold −32; Attack 25 ms; Release 150 ms). **Cleanup:** Noise suppression "RNNoise: remove fans and hum" (Amount 100 %). Invert polarity "Flip the waveform" (no params). **Tone:** EQ "Shape low, mid and high" (Low/Mid/High 0 ±20 dB step 0.5; Mid frequency 1000 100-8000 Hz step 10). **Timing:** Audio delay (Delay 0 0-5000 ms step 5). **Mastering:** Loudness target "Ride the level to a LUFS target" (Target −14 −30..−6 LUFS; Max boost 12 0-24 dB; Speed Slow/Medium/Fast default Slow). True-peak limiter "A brick wall the encoder can’t clip" (Ceiling −1 −12..0 dBTP step 0.1; Release 50 ms). Ducking "Dip everything else while the mic talks" (Triggered by Mic · ES-9 Line/Desktop audio; Amount −12 −40..0 dB; Release 400 10-3000 ms). **Plugins:** CLAP plugin "Host a CLAP audio effect" (Plugin TDR Nova / Airwindows Consolidated / Surge XT Effects). | L798-810 |
| F13 | Seed filter stacks | webcam: Color correction (sat 6), Rounded corners (radius 24), Alpha mask "Cam shape" **off**, Drop shadow. display: Sharpen. alerts: Chroma key **off**, Gain −6, Limiter. ticker: Scroll. logo: Displacement "Lower third pack" strength 12, Glow. out:program: Loudness target, Compressor (2:1, −12), True-peak limiter, Apply LUT `stream-look.cube` 40 **off**. out:youtube: Sharpen 0.15. Scene Desktop + Cam: LUT 60. Intro overlay: CC (contrast 12, sat −8), Glow 30 **off**, Clip to "Cam shape". Just Chatting: LUT teal-orange 45, Sharpen 0.1. BRB: Blur 24. | L812-829 |
| F14 | Sharing | Filters are stored per source/scene in the workspace (`filters`/`filtset`) and apply wherever the source is shown. References share their source's chain. | L768-771, L965-975 |

<a id="stream-stats-panel-simulated"></a>
## 6. Stream stats panel (`kind: ytstats`) — **simulated**
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| Y1 | Status line | A pill (22px, radius 11, 11px/600, letter-spacing 0.6) with a 7px dot. LIVE: bg `rgba(198,47,54,.22)`, border `#C62F36`, dot `#FF5C63`, text "LIVE". Not live: bg `#1F232A`, border `#2E333B`, dot `#7D8590`, "READY". Then the stream title (ellipsis, `#9AA1AC`; default "Building JStream: the canvas editor"), then a mono **uptime** HH:MM:SS, or `--:--:--` when not live. | L409-414, L1157, L1175-1180 |
| Y2 | Tiles | A 2×2 grid. Each tile has bg `#1F232A`, radius 6, padding 8×10: label 11px, value mono 20px, sub 11px `#7D8590`. **Watching now** n / "peak N". **Likes** n / privacy (e.g. "Unlisted"). **Chat** "N/min" / "messages". **Dropped frames** "0" / "0.00% · encoder". When not live, values are "–" and the first sub is "not live". Numbers use en-US grouping. | L416-424, L1181-1186 |
| Y3 | Health | A card with an 18px icon and "Stream health: <label>" plus a note. Not live: **Waiting** (clock, `#9AA1AC`) "YouTube has not received data yet." More than 6 samples below 5000 kbps in the window: **OK** (warning triangle, amber) "Bitrate dipped below 5 Mbps a few times." Otherwise **Excellent** (check circle, `#3FB27F`) "No issues reported by YouTube ingestion." | L426-432, L1167-1170 |
| Y4 | Bitrate chart | Header **"BITRATE · LAST 2 MIN"** with the current value "x.xx Mbps" (mono). SVG viewBox 240×64, height 72px, preserveAspectRatio none, crosshair cursor. A dashed target line at 6000 kbps (`#3A3F48`, 3 3). A baseline `#2E333B`. A blue `#4C8DFF` 2px polyline. Y range = min(4000, data)…max(7000, data). Axis captions: "2 min ago" · "target 6.0 Mbps · dashed" · "now". **Hover:** a vertical line `#9AA1AC`, an 8px blue dot with a 2px `#16191E` ring, and a tooltip "x.xx Mbps · N s ago"/"now". The tooltip anchors left below 30%, right above 70%, centred otherwise. | L434-458, L1159-1209 |
| Y5 | No-data state | A dashed box 72px: "No data yet. Go Live to start the stream." | L459-461 |
| Y6 | Details | Key/value rows (value mono 12px, right-aligned): Video "1920×1080 · 60 fps · H.264"; Audio "AAC · 160 kbps · 48 kHz"; Latency "<Low> (~5 s)" (Ultra-low ~2 s, otherwise ~15 s); Visibility <privacy>; Ingest "rtmps://a.rtmps.youtube.com"; Stream key "jstream-main (reusable)". | L464-468, L1210-1217 |
| Y7 | Simulation | A 1s timer, active only while kind=ytstats and live (`stream.live`, default live). The bitrate seed is 120 samples around 6000 (a sine wave plus noise). Each tick: mean-revert toward 6000 (×0.3) ± 130, a 2% chance of a 900-1600 drop, clamped 3500-6900, window 120. Viewers random walk from 1284 (peak 1310). Likes +1 with 25% chance (from 212). Chat/min ±3 (from 18). Uptime counts up and resets to 0 when not live. | L938, L956-963, L1132-1153 |

<a id="chat-panel-simulated-static"></a>
## 7. Chat panel (`kind: chat`) — **simulated / static**
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| C1 | Layout | A full-height column (min 220px): header, pinned message, scrolling list (min 80px), composer pinned at the bottom. | L549 |
| C2 | Header | An 8px red dot `#FF5C63`, **"YouTube live chat"** (600 `#E6E8EB`), then right-aligned mono **"1,284 watching"** (static). | L550-555 |
| C3 | Pinned | A row with bg `#1F232A`, radius 5, 12px. A pin icon in amber `#E8A33D`, then "Pinned: **Repo is github.com/jeffmhopkins/JStream**". | L556-559 |
| C4 | Message row | Padding 5×6, radius 5, 3px left border. Inline: mono 11px time `#7D8590`, a badge (11px, badge colour), the author (600, per-user colour), the text `#E6E8EB`, line-height 1.4. Badges: BOT `#9AA1AC`, MEMBER `#7FD6A8`, MOD `#4C8DFF`, OWNER `#F2C14E`. **Super Chat**: bg `rgba(232,163,61,.12)`, left edge `#E8A33D`, text like "$5.00 · keep going, this is great". | L562-568, L1797-1807 |
| C5 | Show on stream | A hover button on each message, "Show" (22px, bg `#1F232A`, border, 11px), tooltip "Show this message on stream". When featured: the label is "On stream", the row bg `#22324A` and left edge `#4C8DFF`. Only one message is featured at a time, and clicking again unfeatures it. The overlay effect is not implemented (state only). | L569, L1798-1805 |
| C6 | Composer | An input with placeholder **"Chat as JStream…"** (30px) and a **Send** button (bg `#22324A`, border `#4C8DFF`). Enter or Send appends `{time:'now', author:'JStream', badge:'OWNER' gold, colour #E6E8EB}`. Trims the text and ignores empty input. Top border `#262A31`. | L573-576, L1108-1113, L1878-1887 |
| C7 | Seed messages | 01:21 rustacean_ella (#7FD6A8) "is the compositor in wgpu too?"; 01:21 Nightbot BOT "Repo: github.com/jeffmhopkins/JStream"; 01:22 tom_codes MEMBER (#8AB4FF) "the fence handoff thing was wild"; 01:22 kat.dev (#E8A33D) super chat; 01:23 modmarcus MOD (#B5CEFF) "reminder: be nice in chat"; 01:23 pixelpusher (#E86BA6) "what GPU are you on?". | L901-908 |

<a id="mixer-panel"></a>
## 8. Mixer panel (`kind: mixer`)
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| M1 | Concept | The Mixer shows the **master** (program output). Its channels **fold underneath, folded by default** (`mixOpen` false). Only sources whose audio is on reach the Mixer. | L1781-1795, L1888-1889 |
| M2 | Orientation | Horizontal or vertical meters. `mixorient` prop 'h'/'v' set from the Mixer title bar (the host's flip button: `mixFlip`, icon bars vs lines, tooltip "Meters: vertical · click for horizontal" / "Meters: horizontal · click for vertical"). Until set it follows docking: vertical when docked in a side column (`vertical === 'yes'`). | L1782-1784, L1891-1896 |
| M3 | Master card (horizontal) | Bg `#1F232A`, border, radius 8, padding 10. Row 1: the fold button ("▸/▾" `#7D8590`) with **"Master · program output"** (600) and "N channels" (12px `#7D8590`), tooltip "Show or hide the channels feeding the master". Then the **monitor** button "Hear: after FX" / "Hear: before FX" (transparent, border, `#C9CED6`, 12px, tooltip "What your headphones hear"). Then **"FX N"** (bg `#22324A`, border `#4C8DFF`, tooltip "Filters on the program output") → `ms.openFx`. Row 2: **Loudness**: a 120px label, a 7px bar (track `#22262D`, fill `#3FB27F` width = `ms.short`%, white 2px **target marker at 70%**, title "Target"), and right-aligned mono "<lufs> LUFS" (128px). Row 3: **True peak**: the note "safe" (green `#7FD6A8`) or "over 0 dBTP: add a true-peak limiter" (`#FF8A8F`), and "<tp> dBTP" in the same colour. | L582-600, L1900 |
| M4 | Channel rows (horizontal, when open) | Indented 10px, with a 2px `#2E333B` left rule and 12px padding. Per channel: a label (120px), a 7px level bar (green), and a 128px dB scrub field (−60…+12, step 0.5, 1 dec). Level % = (dB+60)/72. | L601-616, L1791-1795 |
| M5 | Vertical mode | Horizontally scrolling strips. **Master strip** 84px (bg `#1F232A`, border, radius 6): a "▸ Master" fold button (11px/600), "N channels" (10px), a 14×150 meter (fill from the bottom, white target line at 70%, tooltip "Loudness · white line = target"), mono LUFS value + "LUFS", TP value (coloured) + "dBTP", a 60px "FX N" button, and a 60px monitor button "Hear post"/"Hear pre" (10px). When open: a 2px divider, then **channel strips** 64px (bg `#1A1D23`, border `#262A31`): short name (the part before " · ", ellipsis, full name as tooltip), a 10×150 meter, a **+** button (48×20), a 52×24 centred mono input (cursor `ns-resize`, scrubs vertically, drag up = louder), a **−** button, and "dB". | L617-649 |
| M6 | Default channels | Mic · ES-9 Line −3 dB; Desktop audio −8.5 dB; Alerts −12 dB (overridable via the `mixer` prop). If `master.privacy`, the mic label becomes "Mic · ES-9 Line · muted (privacy)". | L1786-1792 |
| M7 | Simulated / static | The meters are static: the level is just the fader value. LUFS, TP, short and tpOver come from the host `master` prop. | L1794, L1900 |

<a id="outputs-panel"></a>
## 9. Outputs panel (`kind: outputs`)
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| U1 | Rows | 30px rows, radius 5, gap 4. First **Program output** (weight 600, bg `#1F232A`, indent 4, status "master", FX button always blue `#22324A`/`#4C8DFF`, tooltip "Filters on everything leaving JStream"). Then each `master.outputs` item indented 18px, weight 400, transparent, with its status text (12px `#9AA1AC`) and an **"FX N"** button (min-width 44, 11px). The button is blue when N>0, otherwise transparent with a `#2E333B` border. Tooltip "Filters for <name> only". Click → that output's filters. | L653-664, L1901-1904 |
| U2 | Footnote | "Program filters apply to every output; each output can add its own after them." (11px `#7D8590`). | L662 |
| U3 | Known output ids | out:program, out:youtube (YouTube stream), out:record (Recording), out:vcam (Virtual camera; video only). Names and statuses come from the host. | L989, L1736 |

<a id="transitions-panel"></a>
## 10. Transitions panel (`kind: transitions`)
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| R1 | Heading | **"PRESETS"** plus the help text: "Pick a preset to edit it. Scenes use the default unless you override it: right-click a scene, or set it in its Properties or the Inspector." | L669-671 |
| R2 | Preset picker | A 30px select (aria "Preset to edit"). Options: "<name> — Cut" or "<name> — <Kind> · <duration> ms", with " (default)" on the default, and finally **"+ New preset…"**, which calls `trans.addPreset()` and edits the new one. Initial preset: the dropdown choice, else the selected scene's "in" override, else the default. | L672-674, L1564, L1590-1597 |
| R3 | Name | "Name" + a 160px text input → `setPreset(id,{name})`. | L677-678 |
| R4 | Type | A 3-column radio segmented control: **Cut / Fade / Move**. The active one has bg `#22324A`. | L679-683, L1553, L1575 |
| R5 | Fields | Not for Cut. **Duration** ms (default 300, 0–10000, step 10). **Move** only: **Stagger per item** ms (0–1000, step 10). Selects (128px): **Easing** (Ease in-out, Ease out, Linear, Spring). **Move** only: **Items in one scene only** (Fade, Scale, Slide). | L684-703, L1576-1583 |
| R6 | Note | "[The default: used unless a scene overrides it. ]Overrides: N in, M out." (11px). | L704, L1584 |
| R7 | Actions | Hidden for the default preset: **Make default** (border, `#C9CED6`) and **Delete** (text `#FF8A8F`). Delete resets the picker. | L705-708, L1585-1587 |
| R8 | Preset colours | `PCOL` (#7FD6A8, #B5CEFF, #8AB4FF, #F5C77E, #B07CFF, #FF8A8F) defined per preset index, but not used in this file's markup. | L1554, L1561 |
| R9 | Empty | Without a `trans` prop, the select is empty and the editor hidden. | L1551-1552 |

<a id="agent-panel-static"></a>
## 11. Agent panel (`kind: agent`) — **static**
<!-- @section
-->

| # | Feature | Requirement | Lines |
|---|---|---|---|
| A1 | Activity log | An ordered list (gap 8). Each item has a mono 11px `#7D8590` meta line "MCP · <relative time>" and an action sentence. Static items: "MCP · 2 min ago" / "Switched to “Desktop + Cam”"; "MCP · 4 min ago" / "Mic gain −1.5 → −3.0 dB". No input, no interaction. | L715-720 |

---

### Not in this file (host responsibilities)
Panel title bars (including the Mixer orientation flip), docking, context-menu contents for scene/object rows and empty areas (`openctx`), the Properties dialog (`openprops`), the full filter dialog and add-filter dialog (`editfilter`, `addfilter`), the tree move/add logic (`treemove`, `treeadd`), Transform/Arrange button definitions (`tx`), the master bus numbers (`master`), and the transitions model (`trans`). See Docking.dc.html for these.
