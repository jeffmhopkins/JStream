<!-- @doc
id = "GETTING-STARTED"
title = "Getting started"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Getting started

How to run a JStream release on Windows, build a first scene, go live, record, and use
the virtual camera, and what to send when something goes wrong. JStream is an **alpha**:
expect rough edges, and keep a second way to stream for anything that matters.

<a id="requirements"></a>
## What you need
<!-- @section
depends_on = ["docs/ARCHITECTURE.md#packaging", "apps/jstream-desktop/README.md#usage", "README.md#platforms"]
-->

- **Windows 11, 64-bit** (build 22000<!--fact:min-windows-build--> or later), with a GPU
  that runs Direct3D 12 ([Platforms](../README.md#platforms)).
- **Media Foundation.** It is part of Windows, except on the N and KN editions, where
  you install the Media Feature Pack from Windows' optional features first. Without it,
  JStream says what is missing and stops.
- **WebView2**, which draws the app's window. Windows 11 comes with it.
- Nothing else: there is no installer, and running JStream needs no administrator rights.

<a id="install"></a>
## Download and first run
<!-- @section
depends_on = ["docs/ARCHITECTURE.md#packaging", "apps/jstream-desktop/README.md#usage"]
-->

1. Download `jstream-v<version>-windows-x64.zip` from the
   [releases page](https://github.com/jeffmhopkins/JStream/releases). Each release lists
   the zip's SHA-256 beside it.
2. Unzip it anywhere, such as `Documents\JStream`, and keep the folder together: the
   `ui` and `browser` folders must stay next to `jstream.exe`.
3. Run `jstream.exe`.

**"Windows protected your PC."** Alpha builds are not code-signed, so SmartScreen warns
about an unknown publisher the first time. Choose **More info**, then **Run anyway**. Do
this only for a zip you downloaded from the releases page.

JStream keeps its settings, scenes and logs in `%APPDATA%\JStream`. To keep them in the
JStream folder instead (to run it from a USB stick), create an empty file named
`portable`, with no extension, next to `jstream.exe`: they then go in its `data` folder.

Your scenes, sources and outputs are one project file. **File › Save project as…** saves
them to a new file, **File › Open project…** and **Open recent** switch to another, and
**File › New project…** starts an empty one. JStream opens the last project at start.
Opening one stops any output that is running, and each project keeps its own stream keys.

<a id="scene"></a>
## Your first scene
<!-- @section
depends_on = ["docs/UI-FEATURES.md"]
-->

A new JStream starts empty, but for four sample labels (Intro / outro, Gameplay, Overlay
and Sponsor) to rename, use or delete. What the program shows (and every output sends) is the scene
on **program**, drawn on the canvas in the middle.
The canvas starts at 1920×1080 and 30 fps. **File › Video settings…** changes its
resolution (a preset such as 1280×720 or 3840×2160, a vertical 1080×1920, or any size
you type) and its frame rate, 30 or 60 fps. Each output scales from that picture.
**Blending** there sets how semi-transparent items and blend modes mix: Browser-style,
the default, matches a web page or a design tool, and Linear light mixes light as it
adds, which makes soft glows and veils brighter.

1. In **Scenes**, click **+** under the list (or right-click and choose **New scene**),
   then type its name.
2. In **Sources**, click **+** under the list (or right-click and choose **Add source…**).
   Pick a type, or type in the search box to find one: display or window capture, a
   webcam, an audio input (your microphone), a media file, an image, a 3D model (a glTF
   or GLB file, or a built-in shape such as a cube), a browser page,
   text (it starts bold with a soft shadow), a colour, a backdrop (a painted studio
   backdrop, generated), a shape (a plate, pill or circle, with an optional gradient),
   a timer (a countdown, stopwatch or clock), or an audio visualiser. Name it and press
   **Create**
   (or pick an existing source to reuse it); its properties open, so you can choose the
   display or window from the list of those open now, the device or the file. In the
   desktop app a file field has a **Browse…** button; a browser page keeps the path as
   text. A display,
   window or webcam shows its live picture at the top of the dialog as you pick it,
   before you press **OK**.
3. Drag the source on the canvas to move it, and its handles to resize it. To crop it,
   hold Alt on a handle, or press C for crop mode (V leaves it). The **Inspector** and
   **Transform** panels hold the exact numbers.
4. Sound sources appear in the **Audio mixer**: check that their meters move, and mute or
   lower them there.
5. To show a scene, click its row in **Scenes**: it goes on program, and the red
   "program" badge marks the one that is live. With **Studio mode** on (in the header),
   a click puts the scene on preview instead, and the button on its row, or
   **Transition** in the Transitions panel, takes it live.

Ctrl+C and Ctrl+V (or **Copy** and **Paste** on a source's right-click menu) copy
sources from one scene to another, and **Duplicate scene** on a scene's right-click menu
(or **Copy** then **Paste scene**, or Ctrl+C then Ctrl+V on its row) copies the whole
scene. A
scene's **Properties** (double-click its row) also set the colour it is drawn over,
which shows wherever no item covers it.

Panels can be moved, hidden and brought back from the **View** menu; **Layout** saves
and resets arrangements. **Help** links to these docs, the release notes and the issue
tracker. **Check for updates** there compares this build with the latest release,
including pre-releases, and links a newer one; nothing is downloaded. **About JStream**
shows the version you run, which an issue report should name. **File › Settings…** can
look once at start and show a notice only when a newer release exists; that check stays
off until you turn it on.

To watch a scene, a source, the program or a multiview on another screen, choose
**Pop out** on a scene's or a source's right-click menu, or a projector under **View**: each opens in a window of its
own, or full screen. Drag a window to the monitor you want and double-click it to fill
that screen; Esc closes it. A multiview tiles the scenes with **Show in multiview** checked, in their
Properties or on their right-click menu.

<a id="live"></a>
## Going live
<!-- @section
depends_on = ["apps/jstream-desktop/README.md#usage", "docs/UI-FEATURES.md"]
-->

JStream streams to any service that takes RTMP or RTMPS (YouTube, Twitch and most
others) with the server address and stream key the service gives you.

1. In **Outputs**, press **+ Add output**, choose **Stream (RTMP)**, paste the
   **server URL** without the key (for example YouTube's `rtmps://` address), paste the
   **Stream key**, and press **Add**. The key is kept in Windows Credential Manager,
   never in JStream's files, and shows only masked. Treat it like a password: never
   paste it into a chat or an issue.
2. Each output is one row in **Outputs**, with its state and a Start button. To change
   it later, double-click the row (or choose **Settings…** in its **⋯** menu); nothing
   changes until you press **OK** or **Apply**.
3. Press **Go Live** at the top right. It starts every stream output whose key is set;
   **End stream** stops them. Each row shows its state, and the stream reconnects by
   itself if the connection drops.

An output sends H.264 at the engine's default rate unless you pick otherwise. To change
the codec or bit rate, or to send a vertical picture (1080 × 1920 for Shorts or TikTok),
make an encoder or a video mix in **Encoders & mixes…**, under the output rows, and pick
it in the output's settings, under **Encoder** or **Video**. A running output takes the
new choice when it next starts.

On Linux, a machine with no hardware H.264 encoder (or an encoder set to **Software
only**) encodes with Cisco's OpenH264, which JStream doesn't ship: **Encoders & mixes…** offers
to download it from Cisco, with Cisco's licence, and nothing downloads until you press
its button. **Delete** beside it removes it again.

Signing in to YouTube (choosing a broadcast, chat, viewer counts) is not in this alpha:
create the broadcast in YouTube Studio and stream to it with its key.

<a id="record"></a>
## Recording
<!-- @section
depends_on = ["apps/jstream-desktop/README.md#usage", "docs/UI-FEATURES.md"]
-->

In **Outputs**, press **+ Add output** and add a **Recording (MP4)** with the file path
to write. In the desktop app, **Browse…** beside that field opens a save dialog; a
browser page keeps the path as text. Then press **Record** at the top. **Stop
recording** finishes the file. A recording runs on its own, whether or not you are live.
It never overwrites a file, so for another take, open its settings (double-click its
row) and give it a new file name. Browse… is there too.

<a id="vcam"></a>
## The virtual camera (optional)
<!-- @section
depends_on = ["docs/adr/0066-portable-windows-distribution-v2.md", "apps/jstream-desktop/README.md#usage"]
-->

The virtual camera lets video-call apps (Teams, Zoom, Meet in a browser) use the program
as a webcam named **JStream Virtual Camera**. It needs a one-time setup that asks for
administrator rights; everything else in JStream works without it.

1. Close JStream. In the JStream folder, open a terminal and run
   `.\jstream-daemon.exe --register-vcam`, and accept the administrator prompt. It copies
   the camera into `%ProgramFiles%\JStream\vcam` and registers it there.
2. Start JStream, add a **Virtual camera** output in **Outputs** (**+ Add output**), and start it.
3. Pick "JStream Virtual Camera" in the other app.

After you update JStream, the virtual camera output's settings may say "registered, but not for this
version": run the same command again to update the registered copy.

<a id="agents"></a>
## Connect an agent (optional)
<!-- @section
depends_on = ["apps/jstream-desktop/README.md#usage", "apps/jstream-cli/README.md#usage"]
-->

An AI agent such as Claude can build and run your scenes through MCP: it gets the same
control you have in the window, except YouTube sign-in and installing plugins, and it
can look at the program with snapshots. Agents can't connect until you turn this on.

1. **Start JStream with `--mcp`.** Right-click `jstream.exe`, choose **Show more
   options** › **Create shortcut**, open the shortcut's **Properties**, and add ` --mcp`
   at the end of **Target**. Start JStream from that shortcut. It then listens for
   agents on `127.0.0.1:4455`, on this computer only, and keeps the password agents use
   in a file named `token` in `%APPDATA%\JStream` (or `data` in portable mode), readable
   only by you. If another program uses port 4455, the window says so: add
   `--port 4460`, or any free port, after `--mcp`, and use that port below.
2. **Tell your agent how to start JStream's MCP proxy**, `jstream-cli.exe` from the same
   folder. For Claude Desktop, open **Settings** › **Developer** › **Edit Config** and add
   this to `claude_desktop_config.json`, with your own folder:

   ```json
   {
     "mcpServers": {
       "jstream": {
         "command": "C:\\Users\\you\\Documents\\JStream\\jstream-cli.exe",
         "args": ["--url", "ws://127.0.0.1:4455", "mcp"],
         "env": { "JSTREAM_TOKEN_FILE": "C:\\Users\\you\\AppData\\Roaming\\JStream\\token" }
       }
     }
   }
   ```

   For Claude Code, in Command Prompt (in PowerShell, write `$env:APPDATA` for
   `%APPDATA%`):

   ```bat
   claude mcp add jstream -e JSTREAM_TOKEN_FILE=%APPDATA%\JStream\token -- "C:\Users\you\Documents\JStream\jstream-cli.exe" --url ws://127.0.0.1:4455 mcp
   ```

   Other MCP clients that speak streamable HTTP can use `http://127.0.0.1:4455/mcp`
   with the token file's contents as a bearer token.
3. **Restart the agent's app** with JStream running. It lists JStream's tools, such as
   `scene`, `source` and `batch`. Ask it to build a scene, then watch the window: what it
   changes shows there, and **Agent activity** lists each change. Your agent may ask
   before going live, changing a stream key or other steps undo can't take back. It can
   undo its own edits, and asking it to is the way back: your **Undo** steps through
   your edits only.

Treat the `token` file like a password: anyone with it controls JStream. To change it,
close JStream, delete the file and start JStream again, then restart the agent's app.

<a id="trouble"></a>
## When something goes wrong
<!-- @section
depends_on = ["apps/jstream-desktop/README.md#usage", "docs/RULES.md#R-SEC-004"]
-->

- **Logs.** `%APPDATA%\JStream\logs` (or `data\logs` in portable mode) holds
  `jstream-desktop.log`, rewritten at each start, and `cef.log`, the browser sources'
  log. Copy them before starting JStream again.
- **Crash reports.** If JStream crashes, it writes `crash-<time>-….txt` in the same
  folder. Stream keys and tokens are removed from it, and nothing is ever uploaded: you
  choose whether to send it.
- **A browser source stays empty.** Check that the `browser` folder is next to
  `jstream.exe`, that the page's address is complete (with `https://`), and look in
  `cef.log` for errors.
- **No video from a webcam or capture.** Another app may hold the device, or Windows'
  camera privacy settings may block desktop apps.
- **Reporting a bug.** Open an
  [issue](https://github.com/jeffmhopkins/JStream/issues/new) with the release version
  (in the zip's name), your Windows version and GPU, what you did, what you expected, and
  the logs. Check them for anything private first. Security problems go to a
  [private report](https://github.com/jeffmhopkins/JStream/security/advisories/new)
  instead ([SECURITY.md](../SECURITY.md)).
