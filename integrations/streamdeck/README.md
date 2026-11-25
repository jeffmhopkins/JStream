<!-- @doc
id = "INTEGRATION-streamdeck"
title = "Stream Deck plugin"
kind = "normative"
status = "active"
review_every = "90d"
-->

# Stream Deck plugin

A plugin for Elgato's Stream Deck app that controls JStream over the network. Its keys run
shows, switch scenes, go to keyframes, show, hide and mute sources, play sequences, turn
effects on and off, start streams and recordings, flip channels and run hotkey actions.
Each key lights up from JStream's live state, so the deck shows what is on program, what
is live, what plays and what is muted. It lives outside the Cargo workspace
([ADR-0232](../../docs/adr/0232-integrations-outside-the-workspace.md)) and speaks only
JStream's public protocol ([jstream-protocol](../../crates/jstream-protocol/README.md)).

<a id="install"></a>
## Install and pair
<!-- @section
covers = ["integrations/streamdeck/io.github.jeffmhopkins.jstream.sdPlugin/manifest.json", "integrations/streamdeck/io.github.jeffmhopkins.jstream.sdPlugin/ui/**"]
-->

The plugin needs the Stream Deck app 7.1 or newer, on Windows 10 or later or macOS 12 or later.
Stream Deck and JStream can run on different computers on the same network.

1. **Install.** Download `jstream-v<version>-streamdeck.streamDeckPlugin` from the
   release on GitHub, next to JStream's own packages, and double-click it. The Stream
   Deck app asks to install it, and a **JStream** group appears in its action list. A
   newer release's plugin installs over the old one. To build it yourself, see
   [Build](#build).
2. **Let devices in.** On the JStream computer, turn on **Allow devices on my network** in
   **Settings › Devices**. It applies at once. The network listener uses port 4456
   unless that was changed. The daemon's flag is `--lan`
   ([ADR-0104](../../docs/adr/0104-network-listener-over-tls.md)). The first time, the
   operating system may ask whether JStream may accept connections.
3. **Connect.** Drag any JStream key onto the deck and open its settings. The panel
   looks for JStream by itself the first time; when it finds exactly one, it fills in its
   address and connects. Otherwise press **Find JStream** and pick the computer, or type
   its name or IP address and press **Connect**.
4. **Allow it in JStream.** Connecting asks JStream to pair this Stream Deck: JStream
   shows "Stream Deck (*computer*) wants to connect" with the address it asked from. Press
   **Allow**, within 2 minutes, and the keys connect
   ([ADR-0235](../../docs/adr/0235-pair-a-device-by-approving-it-on-the-host.md)). The
   Stream Deck trusts the certificate it saw when it asked.

   **Or pair with a code**, which also checks the certificate: press **Use a code
   instead** (the panel offers it by itself when JStream denies the ask, no one answers,
   or JStream is too old to ask). In JStream, open **Settings › Devices** and press
   **Pair a device**. Check that the fingerprint JStream shows matches the one in the
   plugin, then type the 8-character code into the plugin and press **It matches:
   pair**. The code works once and expires after 2 minutes.

Every key shares that one pairing, so this happens once per Stream Deck computer.
JStream lists the Stream Deck under **Paired devices**, where it can be revoked. A revoked
or expired pairing turns the keys' badges red, and the connection line says to pair again.
A certificate other than the paired one stops the keys too, since retrying won't change
it: if JStream renewed its certificate, pair again; otherwise something on the network is
pretending to be JStream. **Forget** clears the pairing on the Stream Deck side only. The
Stream Deck stays paired in JStream until you revoke it under **Settings › Devices ›
Paired devices**.

**Find JStream** lists every JStream that announces itself on the network: one whose
**Allow devices on my network** is on and whose **Let devices find JStream on my network**,
just under it, is on too (it is unless turned off; the daemon announces unless started
with `--lan-quiet`, [ADR-0233](../../docs/adr/0233-announce-the-network-listener-on-mdns.md)).
Finding JStream lets nothing in: pairing still takes Allow in JStream, or the code and
the fingerprint. Typing
the address always works, for example where the network drops multicast.

<a id="keys"></a>
## Keys
<!-- @section
covers = ["integrations/streamdeck/src/actions/**", "integrations/streamdeck/src/lists.ts", "integrations/streamdeck/src/render.ts"]
-->

Each key is picked from live lists in its settings. A two-state key can **Toggle**, or
only turn its target on or off. A key whose target is gone shows a grey **?** badge, and
every key shows a red **!** badge while JStream can't be reached. A press that JStream
refuses flashes Stream Deck's alert.

| Key | Does | Lit while |
|---|---|---|
| **Show** | Restart a show (from its start, running or not: `show.stop`, then `show.play`), Play / Pause / Continue (one key: plays the show, or rehearses it on preview; pauses it while it plays; continues it while it waits at a bookmark or paused), Next and Prev (the next or previous bookmark: `show.skip`, `show.back`), with the show picked or the **Selected show**: the one running, else the one JStream's Show panel has open, else the only one, Stop, go to a bookmark, or go to bookmark 1 to 8 of the running show; Continue only and Pause only remain for keys set up with them (`show.*`; a JStream without shows lists none) | Play / Pause / Continue: lit with the pause glyph while the show plays, ringed with the continue glyph while it waits. Continue only: the show waits for Continue. Pause only: paused. Stop: a show runs |
| **Switch scene** | `scene.switch`, with an optional transition. With a **Keyframe** picked from the scene's own, it switches program to that keyframe scene; from the scene or another of its keyframes, that plays as a Move | on program (red; with a keyframe, while that keyframe is) or preview (green) |
| **Go to keyframe** | With the keyframe's scene, or another of its keyframes, on program: `scene.switch` to the keyframe, a Move with the optional transition. Otherwise `scene.set_keyframe`, which changes the keyframe the scene is drawn through where other scenes place it (ADR-0116). It never creates a keyframe | program shows that keyframe, or, with its family off program, it is the placed keyframe |
| **Show or hide source** | `item.toggle`, `item.show` or `item.hide` on one item of a scene | the item is shown |
| **Mute source** | `source.set_audio` with `muted` | the source is muted (red) |
| **Sequence** | `timeline.play` or `timeline.stop` | it plays (ringed while it waits to start) |
| **Effect on or off** | `filter.set_enabled` on an effect of a source, a scene or an item | the effect is on |
| **Stream or record** | `output.start` or `output.stop` | live (red); ringed while starting, stopping or reconnecting |
| **Channel switch** | `channel.switch` on, off or toggle ([ADR-0183](../../docs/adr/0183-virtual-control-channels.md)) | the channel is on |
| **Run action** | the action a JStream hotkey is bound to, or any action written as JSON | never; it is a trigger |

Mute and effect keys send edits, so each press is one undo step in JStream, under the
Stream Deck's name. Every other key sends an action, which adds nothing to undo.

<a id="design"></a>
## Design
<!-- @section
covers = ["integrations/streamdeck/src/*.ts", "integrations/streamdeck/src/jstream/**"]
depends_on = ["docs/adr/0104-network-listener-over-tls.md", "docs/adr/0044-pairing-remote-browsers.md"]
-->

- **One connection.** The plugin keeps one WebSocket connection to JStream for every key
  (`hub.ts`, `jstream/client.ts`). It says `hello` as `Stream Deck (<computer name>)` at
  protocol 0.169, the oldest minor that has every call it makes, and reconnects after
  1, 2, 5, 10 and then every 20 seconds. A `401` (a revoked or lapsed device) or a
  certificate other than the paired one stops the retries until the person pairs again.
- **Trust.** JStream's network listener has a self-signed certificate
  ([ADR-0104](../../docs/adr/0104-network-listener-over-tls.md)), so the plugin trusts
  its SHA-256 fingerprint: the one the person compared with the one JStream shows when
  pairing with a code, or, when pairing by asking, the one the probe saw (trust on first
  use, [ADR-0235](../../docs/adr/0235-pair-a-device-by-approving-it-on-the-host.md)). Every
  connection, the pairing request included, completes the TLS handshake and checks the
  fingerprint before it writes a byte of HTTP (`jstream/tls.ts`). So no token and no code
  reaches any other server. A mismatch is refused, and not retried, with a message that
  says to pair again if JStream renewed its certificate.
- **Pairing** is ADR-0235's ask, `POST /api/v1/pair/ask` with a name, held until the
  person presses Allow or Deny in JStream (the plugin waits 130 seconds, one ask at a
  time, and none while it is paired; pairing with a code meanwhile abandons the ask,
  which JStream then ends), or ADR-0044's exchange, `POST /api/v1/pair` with the code and a name. Either
  answers with a device token. The token, the fingerprint, the host and the port are kept
  in the plugin's global settings, which the Stream Deck app stores for the plugin on that
  computer. The token admits only this device and can be revoked from JStream at any time.
- **Live state** (`jstream/studio.ts`). After `welcome`, the plugin reads `state.get`,
  `studio.get`, each output's `output.status` and `channel.values`. `state.changed`
  re-reads the collection. `studio.changed`, `timeline.changed`, `scene.keyframe_changed`
  and `show.changed` re-read the studio, gathering bursts over 80 ms.
  `output.state_changed` updates one output. No event reports a channel's switch, so
  `channel.values` is read every second, only while a channel key is on screen. Nor does
  one report the show open in JStream's Show panel, its `show.open` UI setting, so
  `ui_settings.get` reads it every 2 seconds while a show key is on screen, and again
  when one with no show picked is pressed.
- **Discovery** (`jstream/discovery.ts`) asks mDNS for `_jstream._tcp.local` for 2.5
  seconds and lists each instance's name, address and port, as JStream announces them
  (one entry per instance and address, so two JStreams on one computer are both listed)
  ([ADR-0233](../../docs/adr/0233-announce-the-network-listener-on-mdns.md)). Finding
  JStream proves nothing about it: the person's Allow, or the fingerprint check and the
  code, still decide.
- **Key images** are SVG drawn per state (`render.ts`). The plugin sets an image or a
  title only when it changed.

<a id="build"></a>
## Build and test
<!-- @section
covers = ["integrations/streamdeck/package.json", "integrations/streamdeck/test/**", "integrations/streamdeck/scripts/**"]
-->

The plugin is TypeScript on the Stream Deck SDK (`@elgato/streamdeck` 3), bundled by
rollup. It needs Node.js 24 to match the runtime the Stream Deck app gives it; Node.js 22
works for building and testing.

```sh
cd integrations/streamdeck
npm ci
npm run typecheck
npm test                    # unit tests; the daemon tests skip
cargo build -p jstream-daemon
npm run build && JSTREAM_DAEMON=../../target/debug/jstream-daemon npm test
npm run pack                # validates, then writes dist/io.github.jeffmhopkins.jstream.streamDeckPlugin
```

- `test/unit.test.ts` covers the lists, labels, key images, fingerprints and discovery
  answers. `test/version.test.ts` covers the release version stamp.
- `test/daemon.test.ts` runs against a real `jstream-daemon --lan`. It finds the daemon
  by its mDNS announcement, probes the
  certificate, refuses a wrong fingerprint, pairs, and drives a scene, keyframe, item,
  mute, effect, sequence, output and hotkey key's calls while watching the state they
  light from. Last, it revokes the device and checks the refusal. A second test runs a
  show (play, pause, continue, go to a bookmark, stop) and watches `studio.get`'s `show`.
  It skips on a JStream without shows.
- `test/plugin.test.ts` runs the built plugin as the Stream Deck app does, against a
  stand-in for the app's socket. A key appears offline, the property inspector pairs it,
  and a press switches program and lights the key. A second scene key, with a keyframe
  picked, puts program on that keyframe scene, and a Go to keyframe key switches to it
  from its family, or places it while another scene is on program. A show's Play / Pause / Continue
  key plays, pauses and continues it, also from a pausing bookmark, and its Restart and
  Next keys start it over and jump to the next bookmark. A key with no show picked
  follows the show the Show panel has open (`show.open`).

The dependencies are exact pins with the lockfile committed (ADR-0232). Their licences are
checked by hand against `deny.toml`'s allow list, since `cargo deny` doesn't read npm's
lockfile. All are on it except `tslib` (0BSD), a build-only dependency of
`@rollup/plugin-typescript` that isn't in the packaged plugin; 0BSD asks even less than
MIT.

Each release builds the plugin too (`streamdeck` in `.github/workflows/release.yml`,
[runbook](../../docs/runbooks/release.md)): it runs the unit tests, stamps the release's
version on the manifest with `node --experimental-strip-types scripts/set-version.ts
<version>`, which turns a pre-release into Stream Deck's fourth number (`0.1.0-alpha.14`
is `0.1.0.14`, `beta.N` is `100 + N`, `rc.N` is `200 + N` and `0.1.0` is `0.1.0.1000`),
then packs it as `jstream-v<version>-streamdeck.streamDeckPlugin`. The committed manifest
keeps `0.1.0.0`, so a local build never looks newer than a release.

`npm run watch` rebuilds and restarts the plugin in a Stream Deck app on the same computer
(`streamdeck link io.github.jeffmhopkins.jstream.sdPlugin` once first). The images under
`imgs/` come from `scripts/icons.ts` and `scripts/icons-png.mjs`.
