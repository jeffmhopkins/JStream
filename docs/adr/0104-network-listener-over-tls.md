<!-- @doc
id = "ADR-0104"
title = "Devices on the network reach JStream over TLS, with a self-signed certificate they pin"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0104: Devices on the network reach JStream over TLS, with a self-signed certificate they pin

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A control surface such as a Stream Deck, a tablet's browser, or an agent on a second
computer usually sits on another machine on the same network as JStream. None of them can
reach it today:

- **Loopback only.** The daemon binds `127.0.0.1` with no way to change it, and the
  desktop app binds `127.0.0.1` on a port the OS picks
  ([R-SEC-002](../RULES.md#R-SEC-002)).
- **No TLS.** `jstream-server` speaks plain `ws://`. Its TLS listener is open question 1
  in [jstream-server › Design](../../crates/jstream-server/README.md#design).
- **Pairing waits on TLS.** [ADR-0044](0044-pairing-remote-browsers.md) accepts a paired
  device's token only over loopback or TLS, and offers pairing beyond loopback only once
  the server has a TLS listener. [ADR-0047](0047-browser-pairing-page-and-device-cookie.md)'s
  pairing page and device cookie exist only on a TLS listener. Both are built, behind
  `ServerConfig::tls`, and unreachable.

ADR-0044 left one question open: "how that certificate is made and trusted is a separate
decision". A home or studio network has no certificate authority, and a public
certificate needs a domain name and a renewal service. Who a remote client trusts, and
what it takes to listen beyond loopback, is security posture, so this is the
maintainer's call ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision
1. **Network access is opt-in.** It stays off until the person turns it on: the daemon's
   `--lan` flag, or the desktop app's "Allow devices on my network" setting. On, it
   opens a second listener on every interface, on a fixed port the person can change.
   The loopback listener is unchanged, so the host's own UI and tools keep working as
   they do.
2. **The network listener is TLS only.** It never speaks plain `ws://` or `http://`. It
   uses rustls with the ring provider ([R-DEP-006](../RULES.md#R-DEP-006)) and keeps the
   `Host` and `Origin` checks, with the machine's host name, its `.local` name and its
   current network addresses added to the accepted hosts.
3. **The network listener never accepts the control token.** It admits device tokens
   only (ADR-0044's `Authorization` or subprotocol, or ADR-0047's cookie), plus
   `POST /api/v1/pair` while a code is open and `GET /pair`. The control token never
   crosses the network, so losing a device costs only that device's token.
4. **A self-signed certificate, made on the host.** The first time network access is
   turned on, JStream makes an ECDSA P-256 key and a self-signed certificate with
   `rcgen`, already in the workspace. Its names are the ones in 2. The key is kept in the
   data folder in a user-only file, like the token file, and never leaves the machine,
   never appears in a log, event, result or snapshot
   ([R-SEC-001](../RULES.md#R-SEC-001)). It needs no domain, no outside service and no
   money.
5. **Trust is the certificate's fingerprint, checked once by the person.** The host
   shows the certificate's SHA-256 fingerprint beside the pairing code and the address,
   in the same form a browser's certificate viewer shows it.
   - A native client (a Stream Deck plugin, `jstream-cli`, another app) shows the
     fingerprint it was offered before it sends the code, the person confirms it
     matches, and the client pins it with the device token. From then on it refuses any
     other certificate.
   - A browser shows its usual warning once; the person compares the fingerprint and
     accepts it, then pairs on `/pair` (ADR-0047).
6. **Renewing the certificate is explicit.** The certificate is long-lived and is never
   replaced silently. When the network addresses it names change, the host says so and
   the person can renew it, which makes a new key and fingerprint. Every pinned client
   then has to pair again, and the host's device list says so.
7. **Out of scope here.** Announcing JStream on the network (mDNS or similar), a
   certificate from a public authority, and limiting what a device token may do. ADR-0044's
   device token keeps full access; narrowing it would be its own decision.
8. **Rule change.** On acceptance, R-SEC-002 also reads: a listener beyond loopback is
   opt-in, TLS only, and admits device tokens, never the control token. Nothing is built
   before then.

<a id="consequences"></a>
## Consequences
- A Stream Deck plugin, a tablet's browser or a second computer can pair with JStream
  across the network with an 8-character code and a one-time fingerprint check. No
  protocol change is needed: they use the existing API with a device token.
- ADR-0044's remote pairing and ADR-0047's pairing page become reachable. The desktop
  app's server pairs devices while network access is on, so the pairing panel shows
  there too.
- A person on the network who can't see the host's screen can't pair: they need a code
  shown there, and a forged certificate fails the fingerprint check or the pin.
- The fingerprint check relies on the person comparing it. A person who skips it on
  first pairing can be fooled by an attacker already in the middle of that one
  connection; after it, the pin holds.
- Browsers keep showing a "not secure" mark for a self-signed certificate, even after it
  is accepted.
- Turning network access on can raise the operating system's firewall prompt, the first
  time on Windows and macOS.
- `jstream-server` gains a TLS listener behind a feature, the daemon a flag, the desktop
  app a setting and a fixed port, and the UI's pairing panel the fingerprint and address.
