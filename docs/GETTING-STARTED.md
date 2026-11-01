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

<a id="scene"></a>
## Your first scene
<!-- @section
depends_on = ["docs/UI-FEATURES.md"]
-->

A new JStream starts empty, but for four sample labels (Intro / outro, Gameplay, Overlay
and Sponsor) to rename, use or delete. What the program shows (and every output sends) is the scene
on **program**, drawn on the canvas in the middle.

1. In **Scenes**, right-click and choose **New scene**, then type its name.
2. In **Scene objects**, right-click and choose **Add source…**. Pick a type: display or
   window capture, a webcam, an audio input (your microphone), a media file, an image, a
   browser page, text, a colour, or an audio visualiser. Name it and press **Create**
   (or pick an existing source to reuse it); its properties open, so you can choose the
   display or window from the list of those open now, the device or the file. A display,
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

Ctrl+C and Ctrl+V copy sources from one scene to another, and **Duplicate scene** on a
scene's right-click menu (or Ctrl+C then Ctrl+V on its row) copies the whole scene.

Panels can be moved, hidden and brought back from the **View** menu; **Layout** saves
and resets arrangements.

To watch a scene, a source, the program or a multiview on another screen, choose
**Pop out** on a scene's or an object's right-click menu, or a projector under **View**: each opens in a window of its
own, or full screen. Drag a window to the monitor you want and double-click it to fill
that screen; Esc closes it. A multiview tiles the scenes whose Properties have **Show
in multiview** checked.

<a id="live"></a>
## Going live
<!-- @section
depends_on = ["apps/jstream-desktop/README.md#usage", "docs/UI-FEATURES.md"]
-->

JStream streams to any service that takes RTMP or RTMPS (YouTube, Twitch and most
others) with the server address and stream key the service gives you.

1. In **Outputs**, under **Add an output**, choose **Stream (RTMP)**, paste the
   **server URL** without the key (for example YouTube's `rtmps://` address), and press
   **Add**.
2. On the new output, enter the **Stream key**. It is kept in Windows Credential Manager,
   never in JStream's files, and it shows only as "Stream key: set". Treat it like a
   password: never paste it into a chat or an issue.
3. Press **Go Live** at the top right. It starts every stream output whose key is set;
   **End stream** stops them. Each output shows its state, and reconnects by itself if
   the connection drops.

Signing in to YouTube (choosing a broadcast, chat, viewer counts) is not in this alpha:
create the broadcast in YouTube Studio and stream to it with its key.

<a id="record"></a>
## Recording
<!-- @section
depends_on = ["apps/jstream-desktop/README.md#usage", "docs/UI-FEATURES.md"]
-->

In **Outputs**, add a **Recording (MP4)** output with the file path to write, then press
**Record** at the top. **Stop recording** finishes the file. A recording runs on its own,
whether or not you are live.

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
2. Start JStream, add a **Virtual camera** output in **Outputs**, and start it.
3. Pick "JStream Virtual Camera" in the other app.

After you update JStream, the Outputs panel may say "registered, but not for this
version": run the same command again to update the registered copy.

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
