// The header's stats readout (HD-04): the program's frame rate, the frames dropped since
// the page connected and the engine's CPU use, from `stats.tick`'s drops and load. The
// mock engine runs no pipeline, so the test sends the ticks (`statsTicks`).
import { test, expect, connect, statsTicks } from '../fixtures.js';

test('the readout shows fps, dropped frames and CPU from stats.tick', async ({ page, daemon }) => {
  const send = await statsTicks(page);
  await connect(page, daemon);
  const readout = page.locator('.app-header .readout');
  await expect(readout).toHaveText('');
  const tick = (seq, load, dropped = 0) => send({ seq, drops: { capture_frames: dropped }, load });

  // Idle: the first tick gives the dropped count, the second the CPU, and no frame rate.
  tick(1, { elapsed_ms: 1000, program_frames: 0, cpu_ms: 100, cpus: 4 });
  await expect(readout).toHaveText('0 dropped');
  tick(2, { elapsed_ms: 2000, program_frames: 0, cpu_ms: 110, cpus: 4 });
  await expect(readout).toHaveText('0 dropped · CPU <1%');

  // Live: 60 frames and 120 ms of CPU over a second on 4 CPUs, 2 frames dropped.
  tick(3, { elapsed_ms: 3000, program_frames: 60, cpu_ms: 230, cpus: 4 }, 2);
  await expect(readout).toHaveText('60 fps · 2 dropped · CPU 3%');
});
