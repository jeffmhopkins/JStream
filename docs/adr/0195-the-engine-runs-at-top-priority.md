<!-- @doc
id = "ADR-0195"
title = "The engine runs at top priority: the highest GPU scheduling class and a high CPU priority class by default"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0195: The engine runs at top priority: the highest GPU scheduling class and a high CPU priority class by default

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0117](0117-real-time-video-thread-model.md) §9 put the render thread in MMCSS
"Playback" and the mixer in "Pro Audio", ran everything else at normal priority, and left
one question open: "Whether the compositor's GPU queue needs a higher GPU priority is
left to the bench."

Since then the profiles of a 1080p30 show on an integrated GPU have shown where the
limit is. The GPU spends 20 to 36 ms of a 33.3 ms frame on the engine's passes, while
the render thread uses about 6 ms of CPU and the whole process less than one core of
sixteen. A dropped frame there is the GPU falling behind. Windows schedules each
process's GPU work by a per-process class, every process starts in the normal one, and
the desktop compositor, a browser and any other program share that GPU with the engine.

A first switch for this (`JSTREAM_GPU_PRIORITY`) landed off by default, waiting for an
A/B on Windows hardware, because a higher class also gets ahead of the desktop
compositor, which display capture and the UI depend on.

The maintainer then decided the default, on 2026-11-23: "I want you to default to that
priority. This will be the single application running on the computer and it needs top
priority real time". A machine that streams is there to stream. What else it runs
(a browser, a chat window, the desktop) matters less than the program's frames.

<a id="decision"></a>
## Decision
This amends ADR-0117 §9. Its thread classes stand; the process's own classes change.

1. **GPU.** On Windows, the first time the engine opens its GPU device, the process asks
   for the `realtime` GPU scheduling priority class, and for `high` when Windows refuses
   that (it keeps `realtime` for processes that hold a privilege). The log names the
   class that was set, or says that none was.
2. **CPU.** At the same moment the process asks for the `high` CPU priority class, which
   puts all of its threads (render, readback, encode, output, control) ahead of normal
   programs' threads. The MMCSS tasks of the render, mixer and audio device threads stay
   as ADR-0117 has them, above it. The engine never asks for the CPU's `realtime`
   class: a busy thread there can stop the machine's input, disk and audio device
   threads, and the audio threads already have the class meant for them.
3. **Overrides only lower.** `JSTREAM_GPU_PRIORITY` (`realtime`, `high`, `above_normal`,
   `normal`) and `JSTREAM_CPU_PRIORITY` (`high`, `above_normal`, `normal`) name a lower
   class; `normal` makes no call. They are for comparing runs and for a machine the
   engine shares with work that matters as much. They are environment variables, not
   settings in the protocol or a project file: they describe the machine, not a show.
4. **Nothing fails.** A refused class leaves the process as it was, with a warning.
5. **Other processes stay normal.** The plugin hosts and the browser helper are separate
   processes and keep the class they start in. Their real-time threads have their own
   MMCSS registration (ADR-0035).
6. **Other platforms** are unchanged. Linux and macOS have no equal of the GPU class,
   and their thread scheduling stays as it is until a bench there asks for more.

<a id="consequences"></a>
## Consequences
- On a busy GPU the engine's frames go first. Other programs on the machine, the desktop
  compositor among them, get what is left, so a desktop UI or a browser beside a heavy
  show can turn sluggish. That is the trade the decision makes.
- Display and window capture read frames the desktop compositor produces. If the engine
  starves it, captured sources update less often. The Windows hardware run that follows
  this ADR looks for that, and the variables are the way back if it shows.
- Not measured when decided ([R-RT-005](../RULES.md#R-RT-005)): no speed-up is claimed
  here. The same show run with each variable at `normal` and unset gives the numbers,
  from `profile.get`'s GPU times and the output's late and dropped frames.
- Whether an unelevated process is granted `realtime` on the GPU, or only `high`, is
  what the log line reports; the engine does not ask for elevation to get it.
- The classes are the process's for as long as it runs, not only while a show is live.
