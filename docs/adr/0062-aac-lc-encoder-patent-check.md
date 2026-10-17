<!-- @doc
id = "ADR-0062"
title = "AAC-LC encoder patent check: may JStream ship its own encoder?"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0062: AAC-LC encoder patent check: may JStream ship its own encoder?

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0061](0061-source-types-and-decoding-scope-v2.md) chose an AAC-LC encoder written in
Rust in this repository as Linux's AAC source. That choice comes with a gate: the item's
first deliverable is a written check of the patents that read on AAC-LC encoding. The
check must say which of them are still in force where JStream is distributed, and whether
a free, open-source encoder needs a licence, from whom, and at what cost. It goes to the
maintainer as its own maintainer-tier ADR, and no encoder code merges until he accepts its
outcome. This is that ADR (roadmap item LNX3).

It is maintainer tier because its outcome is a licensing decision and possibly a spending
one ([ADR-0043](0043-narrow-maintainer-tier.md), [R-GOV-005](../RULES.md#R-GOV-005)).

**This is not legal advice.** It was written by an agent from public sources, and nobody
qualified to give a legal opinion has reviewed it. It sets out what the sources say, what
could not be confirmed, and the options, so that the maintainer can decide. It does not
claim that any patent is or isn't infringed, valid or enforceable.

### What JStream would ship
An encoder only: no decoder and no HE-AAC. It produces the 48 kHz stereo AAC-LC stream
YouTube's RTMP ingest takes, and it ships free of charge as part of a permissively
licensed app that anyone can download. AAC-LC is the Low Complexity profile of MPEG-2 AAC
(ISO/IEC 13818-7, first published in 1997). MPEG-4 Audio (ISO/IEC 14496-3, first published
in 1999) adopted it as audio object type 2, and the stream FLV carries is signalled that
way. MPEG-4 added one optional tool to the LC object type, perceptual noise substitution
(PNS). An encoder never has to use it.

<a id="findings"></a>
## Findings

### 1. The patent pool: who licenses AAC, and on what terms
- **Via Licensing Alliance (Via LA)**, formerly Via Licensing, administers the AAC pool.
  Its [AAC programme page](https://www.via-la.com/licensing-programs/aac/) lists the
  covered technologies as one licence: AAC-LC, HE-AAC, HE-AAC v2, xHE-AAC, MPEG-D DRC,
  AAC Scalable, AAC-LD, AAC-ELD and AAC-ELD v2. There is no separate AAC-LC licence or
  price. The licensors listed include AT&T, Dolby, ETRI, Fraunhofer, Google, Philips,
  Microsoft, NEC, NTT, NTT DOCOMO, Orange, Panasonic, Samsung, Sony and VoiceAge.
- **Who must license:** "manufacturers or developers of end-user encoder and/or decoder
  products". The fees are per unit: $0.98 for each of the first 500,000 units a year,
  falling to $0.10 at volume, plus a one-time fee of $15,000 ($1,000 for a small entity
  of 15 or fewer employees and under US $1 million in gross revenue). The page lists no
  cap. No fee is due for distributing AAC bit streams, so YouTube and its viewers pay
  nothing for the stream itself.
- **Free distribution is not exempt.** The only full licence text found in public is the
  [AAC Patent License Agreement filed with the SEC](https://www.sec.gov/Archives/edgar/data/1649009/000121390020023370/ea125930ex10-9_siyatamobile.htm)
  (Via Licensing's form agreement, dated June 2018, filed as an exhibit by a licensee).
  Its fees apply to each licensed product "Sold or Otherwise Supplied", "Otherwise Supply"
  includes transfers "without compensation, consideration, or money", and each computer
  with the software installed counts as a separate product. Fees apply only "in which any
  Licensed Patent remains unexpired". Whether the current agreement differs couldn't be
  checked, because Via LA doesn't publish it.
- **The pool's list doesn't map patents to profiles.** Via LA's
  [patent list](https://www.via-la.com/wp-content/uploads/2026/09/26Q3-AAC-Patent-List-website.pdf)
  gives each patent's licensor, country and number. It has no expiry dates, no priority
  dates, and no column saying which profile or tool a patent is essential to. It holds
  hundreds of patents in force, among them roughly 255 US entries and well over 100 each
  for EP and JP, but the list alone can't say which of them, if any, read on AAC-LC.
- **Fraunhofer** still points every AAC profile at the pool. Its
  [AAC-LC page](https://www.iis.fraunhofer.de/en/ff/amm/broadcast-streaming/aaclc.html)
  says "a patent licensing program covering all versions of AAC is administered by Via
  Licensing Alliance", and its
  [patent licensing page](https://www.iis.fraunhofer.de/en/ff/amm/lizenz/patent.html)
  offers the pool or a direct licence. Neither page says anything about AAC-LC patents
  expiring. For MP3, Fraunhofer publicly ended its licensing programme. **No similar
  statement for AAC-LC was found, from Fraunhofer or from any other licensor.**

### 2. Which patents could still read on AAC-LC
- **The core tools are old.** Everything in the MPEG-2 AAC LC tool set was standardised
  by 1997, and PNS by 1999. Utility patents filed after June 1995 run 20 years from filing
  in the US, and 20 years from filing in the EPO's member states and in Japan. A patent
  essential to a tool fixed in 1997 would normally have been filed by then, and so would
  have expired around 2017 to 2019, plus any US term adjustment. **That is an inference
  from the dates, not a patent-by-patent finding.**
- **The longest-lived US pool patents with an early priority date are not LC tools.** A
  community analysis of the pool's list on
  [Doom9's forum](https://forum.doom9.org/showthread.php?p=2027355) looked up every
  patent on Google Patents and listed the US patents with a priority date on or before
  the end of 2003 that are still in force. Their last expiry is 2028-12-09. The analysis
  itself says it "is not legal advice and may not be correct", and 1,120 entries had no
  dates on Google Patents, mostly from countries Google Patents covers poorly. This
  check read the Google Patents pages of nine of the eleven US patents it listed. All nine
  are SBR, parametric stereo, multichannel or band-extension patents:

  | Patent | Holder | Subject | Expiry (Google Patents) |
  |---|---|---|---|
  | [US 9462404](https://patents.google.com/patent/US9462404) | Fraunhofer | Multichannel downmix side information | 2026-05-19 |
  | [US 8014534](https://patents.google.com/patent/US8014534) | Dolby International | Parametric stereo width | 2026-06-29 |
  | [US 7577570](https://patents.google.com/patent/US7577570) | Dolby International | SBR filterbank aliasing | 2026-08-10 |
  | [US 7756711](https://patents.google.com/patent/US7756711) | Panasonic | Frequency-domain band extension | 2026-12-27 |
  | [US 8340302](https://patents.google.com/patent/US8340302) | Philips | Parametric stereo | 2027-08-01 |
  | [US 8116460](https://patents.google.com/patent/US8116460) | Dolby International | High-frequency reconstruction (SBR) | 2027-10-24 |
  | [US 7933415](https://patents.google.com/patent/US7933415) | Philips | Parametric stereo decorrelator | 2027-12-27 |
  | [US 7949057](https://patents.google.com/patent/US7949057) | Panasonic | Band extension | 2028-07-18 |
  | [US 8311809](https://patents.google.com/patent/US8311809) | Philips | Parametric stereo | 2028-12-09 |

  The other two (US 7502743, Microsoft, and US 7447317, Fraunhofer) had expiry dates
  before this check, so they weren't read. For the five whose claims weren't on the page
  read, the subject comes from the description, not from claim 1.
- **Patents filed after 2003** are presumably for the tools standardised later: HE-AAC v2
  and parametric stereo, AAC-LD and ELD, MPEG Surround, xHE-AAC, MPEG-D DRC. **This
  check didn't confirm that none of them reads on AAC-LC.** A later patent can claim an
  improvement that an LC encoder could use, or be declared essential to the whole family.
- **Encoder techniques are a separate risk the pool doesn't cover.** A pool licenses
  patents essential to the standard, which mostly means the bitstream and decoding.
  Ways of building a good encoder, such as psychoacoustic models, quantisation and rate
  loops, and window-switching decisions, can be patented without being essential, and no
  pool would list them. An encoder built from the standard and from literature that is
  old enough avoids anything filed later.

### 3. What other distributors do
- **Fedora (Red Hat)** ships `fdk-aac-free` in its main repositories: Fraunhofer's FDK
  library with the newer high-efficiency profiles stripped, leaving AAC-LC. Red Hat Legal
  cleared it in its
  [package review](https://bugzilla.redhat.com/show_bug.cgi?id=1501522), but what was
  cleared there is the library's *licence* as free software. The patent reasoning was not
  published. Debian's packaging of the same fork, still waiting in Debian's
  [NEW queue](https://ftp-master.debian.org/new/fdk-aac-free_2.0.2-3.html), describes it
  as stripping "newer high efficiency profiles with patent concerns", on the premise that
  "patents no longer apply to this code".
- That is a distributor with a legal department shipping an AAC-LC encoder in source and
  binary form, for free, with no pool licence that it has disclosed. It is evidence of
  how Red Hat assessed the risk for its own distribution, mainly in the US. It is not a
  statement by any patent holder, and it doesn't cover JStream.

### 4. By jurisdiction
- **United States:** the strongest evidence. The analysed early-priority patents still in
  force are all SBR, parametric stereo or multichannel, and the last of them expires on
  2028-12-09. Fedora ships AAC-LC from the US.
- **Europe (EPO member states):** not confirmed. The EP rows of the pool list weren't
  checked one by one, and the community analysis didn't include them. EP terms are 20
  years from filing with no term adjustment, so an EP patent filed with a US counterpart
  usually expires no later than it. That suggests, but doesn't show, that Europe is no
  worse than the US.
- **Japan:** not confirmed, for the same reasons. JP terms are also 20 years from filing.
- **Elsewhere:** the pool list has many entries in China, Korea, Brazil, India and other
  countries, including Brazilian patents expiring as late as 2035, and nobody has mapped
  them to profiles. JStream's releases are downloadable worldwide, so "where JStream is
  distributed" is everywhere unless the maintainer decides otherwise.

### 5. What this check could not confirm
- That no unexpired patent in the pool reads on AAC-LC encoding, in any country. The pool
  doesn't say, and neither does any licensor.
- Whether Via LA would treat a free, open-source AAC-LC-only encoder as needing a licence.
  It wasn't asked, and asking would be a decision for the maintainer.
- The patent terms of the current pool agreement, which isn't public.
- Red Hat's reasoning on patents, beyond the fact that it ships AAC-LC.
- Any patents outside the pool, essential or not.

<a id="options"></a>
## Options
- **A. Build the AAC-LC encoder, AAC-LC only, with fixed limits.** It encodes only with
  tools of the MPEG-2 AAC LC profile as published in 1997, so no PNS and nothing from any
  later profile. Its encoder techniques (psychoacoustic model, quantisation, rate control)
  come from the standard's informative parts and from literature dating from before 2003,
  and the item's PRs cite their source for each one. It ships worldwide with no pool
  licence. This is what ADR-0061 asked for. The risk left is the part of the findings
  that couldn't be confirmed: EP, JP and other countries, and patents filed after 2003.
- **B. Build it, but restrict it.** The same encoder, enabled only where or when the risk
  is lowest: for example only in builds for the US, or only after a stated date. Gating
  by country isn't workable for an app anyone can download and build, and no date found
  here marks the end of AAC-LC risk outside the US. Before 2028-12-09 also isn't a date
  that matters for AAC-LC, because the patents that expire then are parametric stereo.
- **C. Don't build it. Load an encoder the user installed.** The engine loads a system
  AAC library at run time, such as the distribution's `fdk-aac` or Fedora's
  `fdk-aac-free`, the way ADR-0061 loads Cisco's OpenH264. The distribution, not JStream,
  ships the encoder. It needs its own exception in
  [R-DEP-002](../RULES.md#R-DEP-002), it works only where the distribution ships one (not
  Debian's main archive, for example), and fdk-aac's own licence grants no patent rights
  either.
- **D. Take a pool licence.** It costs $15,000 up front ($1,000 as a small entity) plus a
  fee per installed computer. Free distribution isn't exempt, and an open-source app has
  no reliable way to count installs. It is a spending decision, and its terms don't fit a
  free product.
- **E. Don't support AAC on Linux.** Linux can't stream to YouTube's RTMP ingest until a
  new decision.

<a id="decision"></a>
## Decision
**Proposed: option A.** JStream builds its own AAC-LC encoder as ADR-0061 decided, within
these limits:
- **Tools.** The encoder produces the MPEG-4 AAC-LC object type using only the tools of
  the MPEG-2 AAC LC profile as first published (filterbank and window switching, TNS, M/S
  and intensity stereo, scalefactors, quantisation and Huffman coding). It never emits
  PNS, SBR, parametric stereo, or any tool from a later profile. A PR that adds a tool
  outside that list needs a new maintainer-tier ADR.
- **Techniques.** The psychoacoustic model, quantisation loop, rate control and
  window-switching decision follow the standard's informative text or published work from
  before 2003. Each PR in the item names its source in the code, and review checks it.
- **No decoder.** The tests decode with an independent reference decoder in CI only. That
  decoder isn't shipped, and its licence is checked under R-DEP-001 when it is added.
- **No claims.** Neither the app nor its docs says AAC-LC is patent-free. The app's
  licence information says that JStream holds no AAC patent licence, and names Via LA as
  the pool administrator.
- **Re-check.** The pool page and patent list are registered with a TTL. If a licensor
  says a patent reads on AAC-LC, or a licensor or Via LA contacts the project about it,
  the encoder is switched off in the next release and the question returns to the
  maintainer.

Accepting this ADR unblocks the AAC encoder's code under LNX3. Rejecting it, or choosing
B to E, keeps that code from merging and leaves ADR-0061's "Linux streaming waits on a new
decision" in force. A choice of C, D or E is then recorded in a new ADR that supersedes
ADR-0061.

<a id="questions"></a>
## Questions for the maintainer
1. **Is this information enough to decide on, or do you want a legal opinion first?**
   This ADR isn't one. Paying for a patent attorney's opinion would be a spending decision.
2. **Which option: A (recommended), B, C, D or E?**
3. **Distribution:** is worldwide distribution with no country restriction acceptable,
   given that Europe and Japan were inferred from filing dates and not checked patent by
   patent?
4. **Limits:** if A, do you accept the tool limit (MPEG-2 AAC LC tools only, no PNS) and
   the pre-2003 technique rule, or do you want them stricter or looser?
5. **Notice:** if A, is "JStream holds no AAC patent licence" with a pointer to Via LA the
   right wording for the app's licence information, or do you want different wording or
   none?
6. **Contacting Via LA:** do you want the project to ask Via LA whether it considers an
   AAC-LC-only, free, open-source encoder to need a licence? Its answer would bind
   nobody, and the question itself draws attention.

<a id="consequences"></a>
## Consequences
- **If A is accepted:** LNX3's encoder work can start. Its PRs carry the tool and technique
  limits above, and their review checks them. Linux streaming still waits for the encoder
  and an H.264 path, as ADR-0061 says.
- **The decision rests on inference, not proof,** for patents filed after 2003 and for
  every country but the US. That is the residual risk the maintainer accepts with A, and
  this ADR names it rather than hiding it.
- **External sources are tracked.** The pool page, the patent list and the other pages
  cited here are registered in [`external.toml`](../external.toml) with TTLs, so they are
  re-read. A new quarterly patent list replaces the registered one when it is published.
- **No other policy changes.** This ADR satisfies ADR-0061's gate and supersedes nothing.
  `deny.toml` and R-DEP-002 don't change under A.
