<!-- @doc
id = "ADR-0156"
title = "The Linux virtual camera through v4l2loopback"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0156: The Linux virtual camera through v4l2loopback

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0060](0060-linux-platform-support.md) §9 left the Linux virtual camera to its own
decision, because the usual way to make one,
[v4l2loopback](https://github.com/v4l2loopback/v4l2loopback), is a kernel module that
needs root to install and load. Until now Linux's `Backends::vcam` has been `None`.

The forces:
- **Apps read cameras through V4L2.** Browsers (through WebRTC), video-call apps, OBS,
  GStreamer and FFmpeg all open `/dev/videoN` nodes. A camera every app sees is a V4L2
  node; nothing in user space alone makes one.
- **v4l2loopback is the common path.** It is packaged by Ubuntu and Debian
  (`v4l2loopback-dkms`), Arch and Fedora (RPM Fusion), and is what OBS's own Linux
  virtual camera uses. Each device it creates is a node that one writer feeds and any
  number of apps read. With `exclusive_caps=1` a node offers capture only while a writer
  feeds it, which Chromium-based apps need before they list it.
- **Root.** Installing the module and loading it with `modprobe` need root. A PipeWire
  camera node would avoid the module, but few apps read cameras through PipeWire yet,
  so it doesn't reach the apps the camera is for.
- **The engine side is simple.** The engine already hands the sink CPU BGRA frames at
  the canvas size on its own thread (`jstream-core`'s camera thread). A v4l2loopback node
  takes whole frames through `write()`, copying each into its next buffer without
  waiting for a reader.

This is maintainer tier ([ADR-0043](0043-narrow-maintainer-tier.md)): it brings the
virtual camera into Linux's supported scope, which ADR-0060 §9 held back as its own
later decision, and that is product scope, as for the Linux browser source
([ADR-0095](0095-browser-helper-on-linux.md)) and desktop app
([ADR-0141](0141-desktop-app-on-linux.md)) from the same list. It also accepts new
risks in the threat model ([Virtual camera](../security/threat-model.md#vcam)): any
process that can open the node reads the program. It adds no privilege, licence or
dependency: JStream asks for no root and loads no module.

<a id="decision"></a>
## Decision
This amends ADR-0060 §9 for the virtual camera only; the rest of it stands.

1. **The Linux virtual camera writes to a v4l2loopback device.** `LoopbackVcam` in
   `jstream-platform-linux` is Linux's `VcamSink`, and `LinuxBackends` hands it to the
   engine, so a `virtual_cam` output works on Linux as on Windows. The daemon always has
   it; the desktop app has it unless started with `--no-vcam`.
2. **The user installs and loads the module. JStream never runs as root**, never calls
   `sudo`, `pkexec` or `modprobe`, and never writes to `/etc`. A start that finds no
   v4l2loopback device it can write fails with a missing-component error whose remedy
   names the distribution's package and the command that creates JStream's camera:
   `sudo modprobe v4l2loopback devices=1 card_label="JStream Camera" exclusive_caps=1`,
   with how to load it at every boot. The docs give the same steps.
3. **Which device.** A start opens each `/dev/videoN` node the user can open, keeps
   those whose driver is v4l2loopback and that take frames now, and tries them with the
   ones labelled `JStream` (any case) first, then by number. A node another writer holds
   refuses the format, and the next is tried. Nothing is configured, so a camera created
   for another app is used only when no JStream-labelled one is free.
4. **What it writes.** I420 (`V4L2_PIX_FMT_YUV420`) at the canvas size cropped to even
   dimensions, BT.709 limited range, signalled in the format's colour fields, at the
   engine's camera rate (30 fps, set with `VIDIOC_S_PARM`). I420 is the format the apps
   above all read. The conversion is the converter the encoders share
   (`jstream_platform::nv12`), with the chroma planes split out. A frame the driver
   can't take now is dropped; the newest frame wins, as the trait asks.
5. **No feedback loop.** While a camera writes to a node, the webcam list leaves that
   node out, so the program can't be added as its own source.
6. **Tests.** The device choice, the format and the conversion are unit-tested on every
   platform. CI's `vcam-linux` job installs and loads v4l2loopback on an Ubuntu runner,
   starts the sink, and reads frames back with FFmpeg as a webcam, checking their
   colours. It is not required, like `browser-linux`: it depends on the runner's kernel
   building the module. Readers on a real desktop (a browser, a video-call app, OBS) are
   checked by hand.

<a id="consequences"></a>
## Consequences
- **Linux has a virtual camera** that browsers, video-call apps and OBS read, once the
  user has loaded the module.
- **One manual step, with root.** It is the user's, once per machine (or per boot
  without the persistence step), and the error says exactly what to run. Secure Boot
  machines need the DKMS module signed, which the distribution's package arranges.
- **Every app that can open the node sees its frames**, as on Windows. Who can open it
  is set by udev and the session (the `video` group, or the seat's own access), not by
  JStream.
- **A camera held by another writer is never taken over**: it refuses the format, and
  JStream moves to the next device or reports that every device is busy.
- A PipeWire camera node can be added later beside this, when apps read cameras through
  PipeWire; it would be a new decision.
