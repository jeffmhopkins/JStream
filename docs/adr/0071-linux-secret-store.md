<!-- @doc
id = "ADR-0071"
title = "Linux secret store: the Secret Service, else a 0600 file"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0071: Linux secret store: the Secret Service, else a 0600 file

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0060](0060-linux-platform-support.md) §9 left one Linux question open: where
stream keys and OAuth refresh tokens are kept, and what happens where no keyring runs.
It called that a security-posture decision for the maintainer, and
[ADR-0043](0043-narrow-maintainer-tier.md) puts security posture in the maintainer tier.
This ADR answers it. It refines ADR-0060 and doesn't replace any of its decisions.

The forces:
- **The rule.** [R-SEC-001](../RULES.md#R-SEC-001) says secrets live only in the OS
  credential store. Windows has Credential Manager, and macOS has the Keychain
  ([ADR-0063](0063-macos-platform-support.md)). Linux has no store every system runs.
- **The Secret Service.** The desktop's keyring is reached through the
  [Secret Service API](https://specifications.freedesktop.org/secret-service/latest/)
  on the session bus, at `/org/freedesktop/secrets`. GNOME Keyring and KWallet
  implement it. A collection can be locked, and unlocking it may show a prompt the user
  can dismiss.
- **Where no keyring runs.** ADR-0060 makes the daemon, with the web UI in a browser,
  the Linux host for M3. A daemon often runs where there is no keyring: over SSH, under
  a systemd user service started before login, in a container, or in a minimal window
  manager session. There may be no session bus at all.
- **Today.** With no store, `set_key` fails rather than fall back to a file
  ([threat model › Secrets](../security/threat-model.md#secrets)), so Linux can't
  stream to any target that needs a key.
- **Build.** The Linux crate already speaks D-Bus itself, for the ScreenCast portal,
  with no `libdbus` or `-sys` crate (ADR-0060 §3).

The options:
1. **Keyring only.** The safest at rest. A machine without a keyring can't stream to
   a target that needs a key, and the daemon setups above are those machines.
2. **Keyring, else a file**: the Secret Service when one is reachable, and otherwise a
   file only the user can read. Streaming works everywhere, at the cost of a plaintext
   key on disk where no keyring runs.
3. **An encrypted file.** It needs a key kept somewhere. Keeping that key beside the
   file protects nothing. A passphrase breaks unattended daemon starts.

<a id="decision"></a>
## Decision
The maintainer chose option 2, "keyring, else file", on the project's decision card.

**1. The Secret Service first.** The Linux crate implements `StreamKeyStore` and
`OAuthTokenStore` on the Secret Service, through its own D-Bus client.
- Items go in the default collection (`/org/freedesktop/secrets/aliases/default`).
  They carry the lookup attributes `application = "jstream"`, `kind` (`stream-key` or
  `oauth-token`) and `id` (the output or account ID). Their label is the Windows
  credential name, such as `JStream/stream-key/<output id>`, so the desktop's keyring
  manager shows them under JStream.
- Secrets cross the bus with the `plain` algorithm. The session bus belongs to the
  user, and another process of the same user is outside the threat model: it could ask
  the keyring for the item anyway.
- A locked collection is unlocked with `Unlock`, which may show the desktop's prompt.
  If the user dismisses the prompt, the call fails. It never falls back to the file.

**2. The file only when no Secret Service is reachable.** That means there is no
session bus, or nothing provides the Secret Service and the bus can't start it. A
keyring that refuses, is locked, or returns an error is a failed call, never a
reason to use the file.
- The file is `secrets` in `$XDG_CONFIG_HOME/jstream`, which is `~/.config/jstream` by
  default. It holds both kinds of secret, named as in the keyring.
- The folder is created `0700` and the file `0600`. A write goes to a new `0600`
  temporary file in the same folder, which is then renamed over the file. If the file
  is readable by group or others, or another user owns it, the store refuses it with an
  error, as the daemon already refuses a token file that others can read. The store
  never repairs the permissions silently.
- The file is plaintext. Secrets read from it are `Secret`s, zeroed on drop, as from
  any store.

**3. The plaintext copy lasts only while it has to.** When the Secret Service is
reachable and the file still holds secrets, the store moves each one into the keyring
and deletes it from the file. It deletes the file once the file is empty. A secret is
always looked up in the keyring first, and in the file only when the keyring is
unreachable.

**4. The user is told.** While the file holds any secret, the engine reports that the
fallback is active. The UI shows a lasting warning that names the file, says the keys
in it are not encrypted, and says how to start a keyring. The daemon logs the same
warning once at startup, without any secret. The protocol field that carries this is
added with the implementation, under the protocol's usual versioning.

**5. A later option to turn the fallback off.** A follow-up adds an app setting in
`jstream.toml` that makes Linux keyring-only, so that without a keyring `set_key` fails
as it does today. Until then, the fallback is always available.

**6. R-SEC-001 gains one exception.** On Linux,
where no Secret Service is reachable, secrets live in the `0600` file described here.
The exception covers nothing else. Secrets still never appear in collection files,
logs, events, snapshots or MCP responses.

<a id="consequences"></a>
## Consequences
- **Linux can stream with a key everywhere**, including headless daemons and sessions
  without a keyring, once the store and an encoder land.
- **Where no keyring runs, a key is plaintext at rest.** Anyone who can read the user's
  files can read it, and so can anyone who can read a backup or sync of the config
  folder. The threat model records this risk
  ([threat model › Secrets](../security/threat-model.md#secrets)). Its mitigations are
  the `0600` permissions, the warning, moving secrets into the keyring once one runs,
  and the later keyring-only setting.
- **More code to review.** The crate gains a Secret Service client and a file store,
  both behind the existing traits and without new dependencies.
- **Edits made at acceptance:** R-SEC-001 gains the exception in decision 6.
  ARCHITECTURE's persistence section and SECURITY.md's secrets promise name it. The
  threat model's Linux row moves from accepted to built when the store merges.
