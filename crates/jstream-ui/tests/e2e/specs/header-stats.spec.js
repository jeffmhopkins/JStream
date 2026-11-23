// The header's stats readout (HD-04): the program's frame rate, the frames the outputs
// dropped since they started and the engine's CPU use, from `stats.tick`'s drops, outputs
// and load. The mock engine runs no pipeline, so the test sends the ticks (`statsTicks`).
import { test, expect, connect, statsTicks } from '../fixtures.js';

test('the readout shows fps, dropped frames and CPU from stats.tick', async ({ page, daemon }) => {
  const send = await statsTicks(page);
  await connect(page, daemon);
  const readout = page.locator('.app-header .readout');
  await expect(readout).toHaveText('');
  const running = [{ output_id: 'output-1' }];
  const tick = (seq, load, dropped = 0, outputs = []) =>
    send({ seq, drops: { capture_frames: dropped }, load, outputs });

  // Idle: no output runs, so what the pipeline drops for the previews isn't shown (#1690).
  // The second tick gives the CPU, and there is no frame rate.
  tick(1, { elapsed_ms: 1000, program_frames: 0, cpu_ms: 100, cpus: 4 }, 5);
  tick(2, { elapsed_ms: 2000, program_frames: 0, cpu_ms: 110, cpus: 4 }, 9);
  await expect(readout).toHaveText('CPU <1%');

  // Live: an output runs. The count starts at its first tick, without the idle drops.
  tick(3, { elapsed_ms: 3000, program_frames: 60, cpu_ms: 230, cpus: 4 }, 9, running);
  await expect(readout).toHaveText('60 fps · 0 dropped · CPU 3%');
  // 60 frames and 120 ms of CPU over a second on 4 CPUs, 2 frames dropped.
  tick(4, { elapsed_ms: 4000, program_frames: 120, cpu_ms: 350, cpus: 4 }, 11, running);
  await expect(readout).toHaveText('60 fps · 2 dropped · CPU 3%');
});
