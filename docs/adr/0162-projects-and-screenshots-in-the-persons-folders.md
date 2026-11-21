<!-- @doc
id = "ADR-0162"
title = "The desktop app saves projects and screenshots in the person's own folders"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0162: The desktop app saves projects and screenshots in the person's own folders

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0121](0121-projects-open-new-and-save-as.md) decision 6 (the file the
desktop app opens when no recent project loads) and the "state lives in
`%APPDATA%\JStream`" line of [ADR-0066](0066-portable-windows-distribution-v2.md), for
the default project only. Settings, logs, the recent list and every other file the app
keeps stay in the data folder, and the daemon is unchanged.

<a id="context"></a>
## Context
The desktop app's default project is `collection.json` in its data folder,
`%APPDATA%\JStream` on Windows. New project… and Save project as… start beside the open
project, so every project saved without Browse… lands in `AppData\Roaming`, which
Explorer hides (#1559). On macOS and Linux the data folder is hidden too
(`~/Library/Application Support`, `~/.local/share`). People look for their projects in
Documents.

A screenshot (ADR-0096 §3) is saved through a download link. In the app the webview puts
it in Downloads, and nothing says so.

Moving the default project is more than a new path. Its stream keys are stored under a
scope derived from its path (ADR-0121 §5), its `collection` asset references are relative
to its folder (§1), and the recent list names it.

<a id="decision"></a>
## Decision
1. **The default project is `Documents\JStream\My project.json`.** On Windows,
   Documents is the shell's known folder (`FOLDERID_Documents`), so a folder OneDrive
   or the person moved is found where Explorer shows it. On macOS it is `~/Documents`.
   On Linux it is `XDG_DOCUMENTS_DIR` from `user-dirs.dirs`, else `~/Documents`. A
   portable app (ADR-0066's `portable` marker) and a system with no Documents folder
   keep `collection.json` in the data folder, so a portable app stays on its stick. The
   app opens the most recent project at startup if it still loads, and the default
   project otherwise, as ADR-0121 §6 says for `collection.json`.
2. **The old default moves once.** When the app starts with `collection.json` in the data
   folder and nothing at the new default's path, it moves the file before its engine
   starts. The move is a `collection.save_as` the engine makes itself
   (`Engine::move_collection_file`), so it follows ADR-0121 §4 and §5: nothing at the new
   path is replaced, `collection` asset references keep naming the same files, and each
   output's stream key is copied to the new path's scope. Then the old path's keys are
   removed, the old file is renamed `collection.json.moved`, and the recent list names
   the new path where it named the old one, keeping the order. A move that fails is
   logged and the app keeps the old file for that start. The next start tries again.
3. **The app saves a screenshot itself, in Downloads, and says where.** The page hands
   the PNG to the app's window (a Tauri command the main window alone is granted), which
   writes it to Downloads (`FOLDERID_Downloads`, `~/Downloads`, `XDG_DOWNLOAD_DIR`) under
   ADR-0096's name, never replacing a file: a taken name becomes `name (2).png`, and so
   on. The toast says "Saved to `Downloads\<name>.png`" and offers Open and Show in folder.
   Those open only a file the app saved in this run. In a browser a screenshot is still a
   download.

<a id="consequences"></a>
## Consequences
- A project file is in a folder people back up and sync. On Windows, Documents is often
  under OneDrive, so a project saved there syncs as any document does. It holds no
  secret: stream keys stay in the OS credential store (R-SEC-001).
- An app older than this one, started after the move, still opens the moved project
  from the recent list, keys and all, since the keys are under the new path's scope. With
  no recent project it finds no `collection.json` in the data folder and makes an empty
  one, which this app then leaves where it is.
- The daemon has no default project and keeps `--collection`.
- No protocol change: the move is the engine's own save as, and the screenshot commands
  are the window's, which no engine client reaches.
