<!-- @doc
id = "ADR-0092"
title = "The engine lists the installed fonts, so a font is picked by name"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0092: The engine lists the installed fonts, so a font is picked by name

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A text source's `font` (and a chat message's) is an `asset` setting: a root and a path to
a TrueType or OpenType file, or none for the bundled Roboto
([ADR-0061](0061-source-types-and-decoding-scope-v2.md)). To use a font the machine has,
a person types its file's path. OBS offers the installed fonts by name, and the
maintainer asked for a picker like it. The fonts are on the engine's machine, which may
not be the browser's, so only the engine can list them. `capture.targets` is the model:
a query for runtime state the platform holds.

<a id="decision"></a>
## Decision
1. **A query, `font.list` (protocol 0.68),** answers every installed font as a
   `FontInfo`: `remote_family` and `remote_style` from the font's name table (the
   typographic family and subfamily when it has them, the US English Windows entry
   first), and the file's absolute `path`. A collection lists its first font, the one a
   text source draws. The names are the font maker's text, so they are `remote_`
   fields ([ADR-0059](0059-mcp-annotations-and-remote-text.md) §2). It is a read: plugin
   class `read`, an MCP tool of its own (`font`), and `jstream font list`.
2. **The engine reads the OS's font folders** (`jstream-sources::fonts`): on Linux
   `/usr/share/fonts`, `/usr/local/share/fonts`, the user's data `fonts` and `~/.fonts`;
   on macOS `/System/Library/Fonts`, `/Library/Fonts` and `~/Library/Fonts`; on Windows
   `%WINDIR%\Fonts` and the user's `Microsoft\Windows\Fonts`. The walk goes at most 8
   folders deep and 8192 files, follows links to files but not to folders, and reads
   only each file's header, table directory and name table (at most 1 MiB). Names lose
   control characters and are cut at 128 characters. No platform API and no new
   dependency: `skrifa`, which the text source already uses, reads the name table.
3. **Read once, with no lock held.** The first `font.list` reads the folders, outside
   the collection lock as `capture.targets` is, and the engine keeps the list for its
   life. A font installed later shows after a restart.
4. **A font is still named by its path.** A picked font is stored as an `asset` with
   root `absolute`, so the text source, the collection file and every client need no
   change. The Properties form shows a font-only `asset` setting (one that accepts `ttf`
   or `otf` and nothing but font files) as a list: the bundled font, each installed font
   by family and style, and "Other file…", which shows the root and path. A stored file
   the list doesn't name selects "Other file…".

<a id="consequences"></a>
## Consequences
- A person picks a font by name, as in OBS. A collection that names an installed font
  by its absolute path opens on another machine without it, as any absolute asset does.
- Any client that may read can list the font files' paths, which say what fonts are
  installed and the user's home folder. That is less than `capture.targets` already
  shows (window titles), and a path names a file without giving access to it.
- The mock lists two made-up fonts (`MOCK_FONTS`) whose files don't exist.
- A family with many styles lists each one. Grouping them by family is the UI's choice.
