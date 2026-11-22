<!-- @doc
id = "RUNBOOK-RELEASE"
title = "Release runbook"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Release runbook

How a JStream version is numbered, and how a tag becomes a downloadable release. What a
user does with the download is in [Getting started](../GETTING-STARTED.md).

<a id="version"></a>
## The version
<!-- @section
covers = ["Cargo.toml"]
-->

The release version is `[workspace.package] version` in the root `Cargo.toml`, following
[SemVer](https://semver.org/spec/v2.0.0.html). The apps the portable zip ships take it
with `version.workspace = true`: `jstream-desktop`, `jstream-daemon`, `jstream-cli`,
`jstream-vcam-windows`, `jstream-browser-helper` and `jstream-ui`, whose About dialog
shows it. So it is what the daemon's and the
CLI's `--version` print and what the logs and crash reports name. The macOS app's bundle
version is its numbers without the pre-release part, as Apple's keys allow
(`scripts/mac/bundle-app.sh`). Libraries, unpublished, stay at `0.0.0`.

Until the first stable release the version is `0.y.z`, and a build cut for testing has a
pre-release part: `0.1.0-alpha.1`, `0.1.0-alpha.2`, then `0.1.0-beta.1`. Bump it in its
own PR (`chore: release 0.1.0-alpha.2`), with `Cargo.lock` updated by `cargo update -w`.

<a id="cut"></a>
## Cutting a release
<!-- @section
covers = [".github/workflows/release.yml"]
depends_on = ["docs/ARCHITECTURE.md#packaging", "docs/adr/0066-portable-windows-distribution-v2.md"]
-->

1. **Pick the commit.** A commit on `main` whose `after-review` run passed, so its
   `dist`, `dist-linux` and `macos-app` artifacts are the packages the release builds. Run the
   [Windows hardware pass](windows-hardware-pass.md) on that artifact for the features
   the release ships.
2. **Check the version.** The workspace version at that commit is the one to release; if
   it is already released, bump it first ([The version](#version)).
3. **Tag it**, one of two ways. Only the maintainer, or an agent the maintainer asked,
   releases.
   - From a clone: `git tag -a v<version> <commit> -m "JStream <version>"`, then
     `git push origin v<version>`.
   - Without git, to release `main`'s head: on GitHub, **Actions** › **release** ›
     **Run workflow**, branch `main`, **Run workflow** (an agent can start the same run
     through the API). The run takes the tag `v<version>` from `main`'s version and
     `publish` creates it on the commit it built, then the release. Steps 1 and 2 then
     apply to `main`'s head when the run starts.
4. **The workflow** (`.github/workflows/release.yml`) runs on the tag, or on the run:
   - `ui` fails unless the tag is `v` and the workspace version (every shipped app must
     have it) and the tagged commit is on `main`, then builds the web UI for release with
     the same pinned tools as `after-review`'s `dist-ui`. A run started by hand must be on
     `main`, and stops if the version's tag already exists or the check can't tell;
   - `zip` runs `cargo xtask dist --revision v<version>` on `windows-latest` from a clean
     build, so the zip is `jstream-v<version>-windows-x64.zip`, browser helper and CEF
     runtime included, with the DirectX Shader Compiler DLLs it downloads and checks
     against their pinned hashes (FXC compiles shaders when they are missing,
     [ADR-0175](../adr/0175-ship-the-directx-shader-compiler.md)), and writes its SHA-256
     and Windows's part of the release notes: the getting-started link, the unsigned
     warning, the CEF build from `browser\CEF-VERSION.txt`, the DXC release from
     `dxc\DXC-VERSION.txt` and the checksum;
   - `linux` runs `cargo xtask dist-linux` on `ubuntu-latest` from a clean build, with the
     same pinned linuxdeploy, GTK plugin and AppImage runtime as `after-review`'s
     `dist-linux`, checks that `chrome-sandbox` is setuid root and that the AAC notice
     ([ADR-0062](../adr/0062-aac-lc-encoder-patent-check.md)) is in the `.deb`, renames
     the packages `jstream-v<version>-linux-amd64.deb` and
     `jstream-v<version>-linux-x86_64.AppImage`, and writes their SHA-256s and Linux's
     part of the notes;
   - `macos` builds the desktop app and the browser helper with `cef` on `macos-latest`
     (Apple Silicon) and bundles them with `scripts/mac/bundle-app.sh`, CEF's framework
     and helper apps included and ad-hoc signed, as `after-review`'s `macos-app` does
     ([ADR-0157](../adr/0157-browser-sources-on-macos.md)). It starts the app once from
     the bundle until its window's UI finishes the engine's handshake, opens a page
     through the bundled helper, renames the image `jstream-v<version>-macos-arm64.dmg`,
     and writes its SHA-256 and macOS's part of the notes, which say how to open an
     unsigned app and name the CEF build;
   - `publish`, the only job allowed to write to the repository, waits for all three and
     creates the GitHub release for the tag with every package and its `.sha256`, the
     notes (a line on the release, then Windows, macOS and Linux) and GitHub's generated
     list of merged PRs. A version with a pre-release part is marked as a pre-release. On
     a run started by hand it first creates the tag, and fails if the tag appeared since
     `ui` checked, so a release never lands on another commit's tag. If any platform's
     job fails, nothing is published.
5. **Check the release page**: each package downloads and its checksum matches; the zip
   starts on a clean Windows machine, the `.dmg` on a Mac once allowed in Privacy &
   Security, and the `.deb` and AppImage on a Linux desktop
   ([ROADMAP › Beta](../../ROADMAP.md#beta) has each platform's pass).

**Signing.** Windows builds are unsigned until a code-signing certificate is available
([ADR-0066](../adr/0066-portable-windows-distribution-v2.md)); the notes and the guide
say how to get past SmartScreen. The macOS image stays unsigned for good
([ROADMAP › macOS without signing](../../ROADMAP.md#macos-unsigned)), and the Linux
packages are unsigned ([ADR-0141](../adr/0141-desktop-app-on-linux.md)). When one is, signing joins the `zip` job before the zip
is written, and the browser helper's exe and DLL get the same certificate
([ADR-0048](../adr/0048-sandboxed-browser-helper.md)).

**A bad release.** Mark it as a pre-release or delete it on GitHub, fix on `main`, bump
the version and tag again. A tag is never moved to another commit.
