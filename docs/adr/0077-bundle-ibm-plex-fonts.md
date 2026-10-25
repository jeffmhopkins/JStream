<!-- @doc
id = "ADR-0077"
title = "Bundle the IBM Plex fonts with the web UI under the SIL OFL 1.1"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0077: Bundle the IBM Plex fonts with the web UI under the SIL OFL 1.1

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The UI prototype sets the UI in IBM Plex Sans at 13 px and numbers and readouts in IBM
Plex Mono (UI-FEATURES WS-02). It loads them from Google Fonts. The real UI can't do that:
the policy every UI response carries allows fonts from `'self'` only (`font-src 'self'`,
`jstream-server`'s `csp`, [ADR-0045](0045-leptos-web-ui.md)), so a font has to be served
by the engine itself, from the files the UI ships with. So far the stylesheet named Plex
first and fell back to the system's fonts, and WS-02 waited on this decision.

The Plex fonts are © IBM Corp., with the Reserved Font Name "Plex", under the SIL Open
Font License 1.1. JStream's own code is MIT OR Apache-2.0. Shipping a third-party asset
under a different licence is a licensing decision, so this ADR is maintainer tier
([ADR-0043](0043-narrow-maintainer-tier.md), [R-GOV-005](../RULES.md#R-GOV-005)). The
maintainer has said the licensing is OK. This ADR records the decision, and it still
needs the maintainer's `/accept`.

**This is not legal advice.** An agent wrote it from the licence text that ships in IBM's
packages, and nobody qualified to give a legal opinion has reviewed it.

What the OFL 1.1 asks of a redistributor, from its text (shipped as
`crates/jstream-ui/fonts/OFL.txt`):
- The fonts may be bundled and redistributed with any software, free or sold, provided
  that each copy carries the copyright notice and the licence (condition 2). The notice
  and licence can be a stand-alone text file.
- The fonts may not be sold by themselves (condition 1). JStream doesn't sell them.
- A modified version may not use the Reserved Font Name (condition 3). Here, a modified
  version includes one that has been re-subset or converted. JStream ships IBM's files
  unmodified, so the name stays.
- The fonts stay under the OFL (condition 5), and that doesn't extend to anything made
  with them. JStream's code keeps its own licence.

<a id="decision"></a>
## Decision
1. **Bundle these five files, unmodified**, in `crates/jstream-ui/fonts/`:
   - IBM Plex Sans in weights 400, 600 and 700
   - IBM Plex Mono in weights 400 and 600

   They are IBM's own `split/woff2` Latin-1 subsets, which cover the UI's languages and
   the weights `style.css` uses, from the npm packages `@ibm/plex-sans` 1.1.0 and
   `@ibm/plex-mono` 2.5.0. Each file is about 20 KB, about 100 KB for all five. Any other
   weight, style or script needs a change to this list, not a new ADR.
2. **Ship the licence beside them** as `fonts/OFL.txt`, IBM's copy of the OFL 1.1 with
   the copyright and Reserved Font Name line. trunk copies `fonts/` into the build as it
   is, so every build the daemon or the desktop app serves carries the licence next to
   the fonts, at `/fonts/OFL.txt`.
3. **Serve them from the page's own origin**, as `@font-face` rules in `style.css` with
   `font-display: swap`. The CSP is unchanged, and the system font shows until Plex loads.
4. **Don't modify them.** Re-subsetting or converting them would make a Modified
   Version, which can't be called "IBM Plex". To do that, a new ADR would have to rename
   the fonts.

<a id="consequences"></a>
## Consequences
- WS-02 is built. The UI looks like the prototype on every platform, without depending
  on the fonts the OS has, and without a network fetch, so it works offline and leaks
  nothing to a font CDN.
- The repository holds about 100 KB of binary font files and one more licence text. A
  release's notice of third-party licences, when there is one, names IBM Plex under the
  OFL 1.1.
- An update to newer Plex files replaces them unmodified and keeps `OFL.txt`.
