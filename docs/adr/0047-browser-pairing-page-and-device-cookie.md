<!-- @doc
id = "ADR-0047"
title = "A browser pairs on a token-free page and keeps its device token in a cookie"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0047: A browser pairs on a token-free page and keeps its device token in a cookie

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0044](0044-pairing-remote-browsers.md) lets a remote browser pair with a one-time
code and get its own device token, presented "as it presents the control token":
`Authorization`, or the WebSocket subprotocol `jstream.token.<token>`. Two things stop a
browser from doing that on its own:

- **It can't reach a form.** Every UI path is behind the token
  ([R-SEC-002](../RULES.md#R-SEC-002)), so a browser without one gets the `Basic`
  challenge before any page that could ask for the code
  ([jstream-server › Design](../../crates/jstream-server/README.md#design)).
- **It can't present the device token on page loads.** A browser sends `Basic`
  credentials only after the user types them, and a page can't set `Authorization` on
  a navigation. Pairing would end with the user pasting a 64-character token into the
  password prompt, which pairing exists to avoid.

How a browser is admitted is security posture, so it is the maintainer's call
([ADR-0043](0043-narrow-maintainer-tier.md)). The maintainer chose a cookie over serving
the UI bundle without a token.

<a id="decision"></a>
## Decision
- **A token-free pairing page.** `GET /pair` answers without a token, after the `Host`
  and `Origin` checks. It is a small static page built into `jstream-server`, not part
  of the UI bundle: a code field, a device-name field and one inline script under the
  same nonce CSP as the UI. It holds no engine data and no secret. A UI path refused for
  a missing token links to it from the `401` body, beside the `Basic` challenge.
- **The exchange sets a cookie.** When the page posts a right code to
  `POST /api/v1/pair`, the answer also sets the device token as a cookie:
  `jstream_device=<token>; HttpOnly; SameSite=Strict; Path=/`, plus `Secure` and the
  `__Host-` name prefix once the listener has TLS. A script can't read it, and the
  browser never sends it on a cross-site request. The JSON answer native clients get is
  unchanged.
- **The cookie is a third way to present a device token.** The gate reads it only when
  neither `Authorization` nor the token subprotocol is present, and only for device
  tokens, never the control token. `Host` and `Origin` are still checked first, so a
  foreign page is refused even if a browser sent the cookie. Revoking the device, or its
  30-day lapse, makes the cookie worthless; the server never needs to clear it.
- **The UI starts pairing.** The host's UI gains a pairing panel
  (`jstream-ui/src/pairing.rs`): it calls `pairing.start`, shows the code until it is
  used or expires, and lists and revokes devices (`device.list`, `device.revoke`).
- **Never logged.** The cookie is a header and is never logged, like every header
  ([R-SEC-001](../RULES.md#R-SEC-001)).
- **Rule change.** On acceptance, R-SEC-002 also excepts `GET /pair`, and ADR-0044's
  "presents it as it presents the control token" gains the cookie, for device tokens
  only. Nothing is built before then.

<a id="consequences"></a>
## Consequences
- A remote browser pairs by opening `/pair`, typing the code shown on the host, and
  landing in the UI, which keeps working across reloads until the device is revoked.
- A second request is answered without a token. It serves a fixed page and changes no
  state.
- A cookie is ambient credential, like the cached `Basic` password: the `Origin` check,
  `SameSite=Strict` and `post`'s `application/json` requirement are what keep foreign
  pages out.
- Cookies are scoped to the host name, not the port, so over plain HTTP another service
  on the same host could receive the cookie if the browser visits it. Device tokens
  already travel only over loopback or TLS (ADR-0044); with TLS, `Secure` and `__Host-`
  narrow this to HTTPS services on that name.
- Browser pairing is useful mainly beyond loopback, so in practice it waits on the
  server's TLS listener, as ADR-0044's pairing does.
