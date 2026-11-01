// The Stream stats panel (ST-01): one output's pill, uptime, bitrate, dropped frames,
// health and bitrate chart, from `stats.tick`'s `outputs` (protocol 0.58). The mock
// engine runs no pipeline, so the test sends the ticks (`statsTicks`).
import { test, expect, connect, showPanel, statsTicks } from '../fixtures.js';

/** Adds the Stream stats panel from the View menu and returns it. */
async function openStats(page) {
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  await page.getByRole('menu', { name: 'View' }).getByRole('menuitemcheckbox', { name: /Stream stats/ }).click();
  await page.keyboard.press('Escape');
  return page.getByRole('tabpanel', { name: 'Stream stats' });
}

test('the panel follows a running output from its ticks', async ({ page, daemon }) => {
  const send = await statsTicks(page);
  await connect(page, daemon);
  const stats = await openStats(page);
  await expect(stats).toContainText('No outputs yet');

  // A recording, which needs no stream key, started from the Outputs panel.
  await showPanel(page, 'Outputs');
  const form = page.locator('form.new-output');
  await form.getByRole('combobox').selectOption('record');
  await form.getByRole('textbox').fill('/tmp/stats.mp4');
  await form.getByRole('button', { name: 'Add' }).click();
  const card = page.locator('li.output').filter({ has: page.getByText('output-1', { exact: true }) });
  await expect(card.locator('.badge')).toHaveText('Stopped');

  await expect(stats.locator('.stats-pill')).toHaveText('READY');
  await expect(stats.locator('.stats-uptime')).toHaveText('--:--:--');
  await expect(stats).toContainText('Record to /tmp/stats.mp4');
  await expect(stats).toContainText('H.264 · 2500 kbps CBR');

  await card.getByRole('button', { name: 'Start' }).click();
  await expect(card.locator('.badge')).toHaveText('Live');
  await expect(stats.locator('.stats-pill')).toHaveText('LIVE');
  await expect(stats.locator('.stats-nodata')).toBeVisible();

  const output = (seq, fields) => send({
    seq,
    drops: {},
    outputs: [{ output_id: 'output-1', ...fields }],
  });
  output(1, { uptime_ms: 3_723_000, bytes: 45_750_000, frames: 3_660, kbps: 2_480, health: 'good' });
  await expect(stats.locator('.stats-uptime')).toHaveText('01:02:03');
  const tiles = stats.locator('.stats-tile');
  await expect(tiles.nth(0)).toContainText('2.48 Mbps');
  await expect(tiles.nth(0)).toContainText('target 2.50 Mbps');
  await expect(tiles.nth(1)).toContainText('0.00% this run');
  await expect(stats.locator('.stats-health')).toContainText('Stream health: Excellent');
  await expect(stats.locator('.stats-chart polyline')).toHaveCount(1);
  await expect(stats).toContainText('45.8 MB · 3660 frames');

  output(2, {
    uptime_ms: 3_724_000, bytes: 46_000_000, frames: 3_690, kbps: 2_000,
    dropped_frames: 30, dropped_packets: 2, health: 'poor',
  });
  await expect(stats.locator('.stats-health')).toContainText('Stream health: Poor');
  await expect(tiles.nth(1).locator('.value')).toHaveText('30');
  await expect(tiles.nth(1)).toContainText('0.81% this run');

  await card.getByRole('button', { name: 'Stop' }).click();
  await expect(stats.locator('.stats-pill')).toHaveText('READY');
  await expect(stats.locator('.stats-uptime')).toHaveText('--:--:--');
  await expect(stats.locator('.stats-health')).toContainText('Stream health: Waiting');
});
