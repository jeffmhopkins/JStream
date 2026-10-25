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
`jstream-vcam-windows` and `jstream-browser-helper`. So it is what the daemon's and the
CLI's `--version` print and what the logs and crash reports name. Libraries, unpublished,
stay at `0.0.0`.

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
   `dist` artifact is the same zip the release builds. Run the
   [Windows hardware pass](windows-hardware-pass.md) on that artifact for the features
   the release ships.
2. **Check the version.** The workspace version at that commit is the one to release; if
   it is already released, bump it first ([The version](#version)).
3. **Tag it.** `git tag -a v<version> <commit> -m "JStream <version>"`, then
   `git push origin v<version>`. Only the maintainer, or an agent the maintainer asked,
   pushes a release tag.
4. **The workflow** (`.github/workflows/release.yml`) runs on the tag:
   - `ui` fails unless the tag is `v` and the workspace version (every shipped app must
     have it) and the tagged commit is on `main`, then builds the web UI for release with
     the same pinned tools as `after-review`'s `dist-ui`;
   - `zip` runs `cargo xtask dist --revision v<version>` on `windows-latest` from a clean
     build, so the zip is `jstream-v<version>-windows-x64.zip`, browser helper and CEF
     runtime included, and writes its SHA-256 and the release notes: the getting-started
     link, the unsigned warning, the CEF build from `browser\CEF-VERSION.txt` and the
     checksum;
   - `publish`, the only job allowed to write to the repository, creates the GitHub
     release for the tag with the zip and its `.sha256`, those notes and GitHub's
     generated list of merged PRs. A version with a pre-release part is marked as a
     pre-release.
5. **Check the release page**: the zip downloads, its checksum matches, and it starts on
   a clean Windows machine.

**Signing.** Releases are unsigned until a code-signing certificate is available
([ADR-0066](../adr/0066-portable-windows-distribution-v2.md)); the notes and the guide
say how to get past SmartScreen. When one is, signing joins the `zip` job before the zip
is written, and the browser helper's exe and DLL get the same certificate
([ADR-0048](../adr/0048-sandboxed-browser-helper.md)).

**A bad release.** Mark it as a pre-release or delete it on GitHub, fix on `main`, bump
the version and tag again. A tag is never moved to another commit.
