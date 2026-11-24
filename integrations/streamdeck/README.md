<!-- @doc
id = "INTEGRATION-streamdeck"
title = "Stream Deck plugin"
kind = "normative"
status = "active"
review_every = "90d"
-->

# Stream Deck plugin

A plugin for Elgato's Stream Deck app that controls JStream over the network. Its keys run
shows, switch scenes, place keyframes, show, hide and mute sources, play sequences, turn
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

1. **Install.** Double-click `io.github.jeffmhopkins.jstream.streamDeckPlugin`. The Stream
   Deck app asks to install it, and a **JStream** group appears in its action list. The
   file is built by `npm run pack` (see [Build](#build)).
2. **Let devices in.** On the JStream computer, turn on **Allow devices on my network** in
   **Settings › Devices**, then restart JStream. The network listener uses port 4456
   unless that was changed. The daemon's flag is `--lan`
   ([ADR-0104](../../docs/adr/0104-network-listener-over-tls.md)). The first time, the
   operating system may ask whether JStream may accept connections.
3. **Connect.** Drag any JStream key onto the deck and open its settings. Under the
   connection line, press **Find JStream** and pick the computer, or type its name or IP
   address. Then press **Connect**. The plugin shows the certificate fingerprint JStream
   offered.
4. **Pair.** In JStream, open **Settings › Devices** and press **Pair a device**. Check
   that the fingerprint JStream shows matches the one in the plugin, then type the
   8-character code into the plugin and press **It matches: pair**. The code works once
   and expires after 2 minutes.

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
Finding JStream lets nothing in: pairing still takes the code and the fingerprint. Typing
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
| **Show** | Play a show (or rehearse it on preview), Continue, Pause, Stop, Back, Skip, go to a bookmark, or go to bookmark 1 to 8 of the running show (`show.*`; a JStream without shows lists none) | Play: the show plays (ringed while it waits). Continue: the show waits for Continue. Pause: paused. Stop: a show runs |
| **Switch scene** | `scene.switch`, with an optional transition | on program (red) or preview (green) |
| **Place keyframe** | `scene.set_keyframe` on the keyframe's scene, with an optional transition | that keyframe is placed |
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
  its SHA-256 fingerprint, which the person compared with the one JStream shows. Every
  connection, the pairing request included, completes the TLS handshake and checks the
  fingerprint before it writes a byte of HTTP (`jstream/tls.ts`). So no token and no code
  reaches any other server. A mismatch is refused, and not retried, with a message that
  says to pair again if JStream renewed its certificate.
- **Pairing** is ADR-0044's exchange: `POST /api/v1/pair` with the code and a name, which
  answers with a device token. The token, the fingerprint, the host and the port are kept
  in the plugin's global settings, which the Stream Deck app stores for the plugin on that
  computer. The token admits only this device and can be revoked from JStream at any time.
- **Live state** (`jstream/studio.ts`). After `welcome`, the plugin reads `state.get`,
  `studio.get`, each output's `output.status` and `channel.values`. `state.changed`
  re-reads the collection. `studio.changed`, `timeline.changed`, `scene.keyframe_changed`
  and `show.changed` re-read the studio, gathering bursts over 80 ms.
  `output.state_changed` updates one output. No event reports a channel's switch, so
  `channel.values` is read every second, only while a channel key is on screen.
- **Discovery** (`jstream/discovery.ts`) asks mDNS for `_jstream._tcp.local` for 2.5
  seconds and lists each instance's name, address and port, as JStream announces them
  ([ADR-0233](../../docs/adr/0233-announce-the-network-listener-on-mdns.md)). Finding
  JStream proves nothing about it: the fingerprint check and the code still decide.
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
  answers.
- `test/daemon.test.ts` runs against a real `jstream-daemon --lan`. It finds the daemon
  by its mDNS announcement, probes the
  certificate, refuses a wrong fingerprint, pairs, and drives a scene, keyframe, item,
  mute, effect, sequence, output and hotkey key's calls while watching the state they
  light from. Last, it revokes the device and checks the refusal. A second test runs a
  show (play, pause, continue, go to a bookmark, stop) and watches `studio.get`'s `show`.
  It skips on a JStream without shows.
- `test/plugin.test.ts` runs the built plugin as the Stream Deck app does, against a
  stand-in for the app's socket. A key appears offline, the property inspector pairs it,
  and a press switches program and lights the key.

The dependencies are exact pins with the lockfile committed (ADR-0232). Their licences are
checked by hand against `deny.toml`'s allow list, since `cargo deny` doesn't read npm's
lockfile. All are on it except `tslib` (0BSD), a build-only dependency of
`@rollup/plugin-typescript` that isn't in the packaged plugin; 0BSD asks even less than
MIT.

`npm run watch` rebuilds and restarts the plugin in a Stream Deck app on the same computer
(`streamdeck link io.github.jeffmhopkins.jstream.sdPlugin` once first). The images under
`imgs/` come from `scripts/icons.ts` and `scripts/icons-png.mjs`.
