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
- **Only on a TLS listener.** Everything below exists only on a listener with TLS,
  where ADR-0044 offers pairing beyond loopback. A browser on the host itself has the
  token file and needs none of it. Elsewhere `GET /pair` is 404 and the `401` body has
  no link to it.
- **A token-free pairing page.** `GET /pair` answers without a token, after the `Host`
  and `Origin` checks. It is a small static page built into `jstream-server`, not part
  of the UI bundle: a code field, a device-name field and one inline script under the
  same nonce CSP as the UI. It holds no engine data and no secret, and changes no state.
  A UI path refused for a missing token links to it from the `401` body, beside the
  `Basic` challenge. `/pair` is reserved: the UI's route fallback never serves it.
- **The browser exchange sets a cookie, and the page never sees the token.** The page
  posts the code and name to `POST /api/v1/pair` with `"browser": true`. A right code is
  then answered with the device ID only, and the device token is set as the cookie
  `__Host-jstream_device=<token>; Secure; HttpOnly; SameSite=Strict; Path=/;
  Max-Age=2592000` (30 days, ADR-0044's lapse). No script, the page's own included,
  can read it. An exchange without `"browser": true` (a native client) is answered as
  ADR-0044 says, with the token in the body and no cookie.
- **The cookie is a third way to present a device token.** The gate reads only the
  `__Host-jstream_device` name, never a plain `jstream_device` another service could
  plant, and refuses a request that carries it more than once. It reads it only when
  neither `Authorization` nor the token subprotocol is present, and only for device
  tokens, never the control token. A request admitted by the cookie must also carry
  `Sec-Fetch-Site: same-origin` or `none`, besides passing the `Host` check and the
  `Origin` check when `Origin` is present. So a page on another port of the same host,
  or on a sibling subdomain (same-site, so the browser sends the cookie), is refused
  even on a `GET` without `Origin`. Each use that renews the device's 30-day lapse also
  re-sets the cookie with a fresh `Max-Age`, at most once a day. Revoking the device, or
  its lapse, makes the cookie worthless; the server never needs to clear it.
- **The UI starts pairing.** The host's UI gains a pairing panel
  (`jstream-ui/src/pairing.rs`): it calls `pairing.start`, shows the code until it is
  used or expires, and lists and revokes devices (`device.list`, `device.revoke`). It is
  hidden when those calls answer `unsupported`, as they do on a session with no
  listener behind it, such as the desktop app's.
- **Never logged.** The cookie is a header and is never logged, like every header
  ([R-SEC-001](../RULES.md#R-SEC-001)).
- **Rule change.** On acceptance, R-SEC-002 also excepts `GET /pair` on a TLS listener,
  and ADR-0044's "presents it as it presents the control token" gains the cookie, for
  device tokens only. Nothing is built before then.

<a id="consequences"></a>
## Consequences
- A remote browser pairs by opening `/pair`, typing the code shown on the host, and
  landing in the UI. It stays signed in across reloads and browser restarts while it is
  used at least once every 30 days, until the device is revoked.
- A second request is answered without a token. It serves a fixed page and changes no
  state.
- A cookie is an ambient credential, like the cached `Basic` password. The
  `Sec-Fetch-Site` and `Origin` checks, `SameSite=Strict` and `post`'s
  `application/json` requirement are what keep other pages out. A browser that sends no
  `Sec-Fetch-Site` can't use the cookie and falls back to the `Basic` prompt.
- Cookies are scoped to the host name, not the port. `Secure` and `__Host-` keep the
  cookie to HTTPS on that exact name, so only an HTTPS service on another port of the
  same name could receive it.
- Browser pairing waits on the server's TLS listener, as ADR-0044's remote pairing
  does. Until then a remote browser can't pair.
