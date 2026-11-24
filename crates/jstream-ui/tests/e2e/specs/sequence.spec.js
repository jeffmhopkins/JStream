// The Sequence panel against the mock engine, which stores and plays timelines but
// keeps no clock (jstream-control's README): making a sequence, its lanes and points,
// Play and Stop, and clicks that come before the engine has answered the last one.
import { test, expect, connect, showPanel } from '../fixtures.js';

/** Routes the page's WebSocket: every request it sends is recorded, and every message
 * the engine sends waits `delay` ms first, as a busy engine or a large collection makes
 * it wait. Returns `sent(type)`, the edits and actions of that type sent so far. Call it
 * before `connect`. */
async function slowEngine(page, delay) {
  const requests = [];
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      if (typeof message === 'string') {
        try {
          requests.push(JSON.parse(message));
        } catch {
          // Not JSON: nothing to record.
        }
      }
      server.send(message);
    });
    server.onMessage((message) => setTimeout(() => ws.send(message), delay));
  });
  const find = (value, type, out) => {
    if (Array.isArray(value)) value.forEach((v) => find(v, type, out));
    else if (value && typeof value === 'object') {
      if (value.type === type) out.push(value);
      Object.values(value).forEach((v) => find(v, type, out));
    }
    return out;
  };
  return { sent: (type) => find(requests, type, []) };
}

/** Shows the Sequence panel with room for its lanes, makes a sequence in the selected
 * scene (Main) and adds a lane on a new channel `name`. Returns the panel. */
async function sequenceWithLane(page, name) {
  // The default layout doesn't show it: View shows it, in the bottom dock.
  await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
  await page.getByRole('menu', { name: 'View' }).getByRole('menuitemcheckbox', { name: /Sequence/ }).click();
  await page.keyboard.press('Escape');
  await showPanel(page, 'Sequence');
  // The bottom dock opens short: pull it up so the lanes show.
  const splitter = page.getByRole('separator', { name: 'Resize bottom dock' });
  const box = await splitter.boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, box.y - 400, { steps: 4 });
  await page.mouse.up();
  const panel = page.locator('.seq-panel');
  // A sequence belongs to the selected scene: wait for the first snapshot to pick one.
  await expect(page.locator('li.tree-row', { hasText: 'Main' }).first()).toHaveClass(/selected/);
  await panel.getByRole('button', { name: '+ New sequence' }).click();
  await expect(panel.getByRole('combobox', { name: 'Sequence', exact: true })).toHaveValue('seq_1');
  await panel.getByRole('button', { name: /Add lane/ }).click();
  await page.getByRole('searchbox', { name: 'Search channels and settings' }).fill(name);
  await page.getByRole('button', { name: `+ New channel “${name}”` }).click();
  await expect(panel.locator('.seq-lane-name', { hasText: name })).toBeVisible();
  return panel;
}

test.describe('Sequence panel', () => {
  test.use({ viewport: { width: 1600, height: 1000 } });

  // Each click is built on what the one before it did, even while the engine hasn't
  // answered: two quick double-clicks add two points, two clicks on M leave the lane
  // as it was, and a second click on Play stops what the first started instead of
  // starting it again.
  test('a click before the engine answers the last one builds on it', async ({ page, daemon }) => {
    const calls = await slowEngine(page, 300);
    await connect(page, daemon);
    const panel = await sequenceWithLane(page, 'Glow');
    const lane = panel.locator('.seq-lane:not(.seq-events)').first();
    // The new lane holds its channel's level at its end: one point.
    await expect(lane.locator('.seq-dot')).toHaveCount(1);

    const area = await lane.locator('.seq-area').boundingBox();
    await page.mouse.dblclick(area.x + area.width * 0.25, area.y + area.height * 0.5);
    await page.mouse.dblclick(area.x + area.width * 0.5, area.y + area.height * 0.2);
    // Shown at once, before the engine answers.
    await expect(lane.locator('.seq-dot')).toHaveCount(3, { timeout: 250 });
    // And kept once it has: the second edit carried the first point.
    await page.waitForTimeout(1000);
    await expect(lane.locator('.seq-dot')).toHaveCount(3);
    const sets = calls.sent('timeline.set');
    expect(sets.at(-1).timeline.lanes[0].points).toHaveLength(3);

    const mute = lane.getByRole('button', { name: 'M', exact: true });
    await mute.click();
    await mute.click();
    await page.waitForTimeout(1000);
    await expect(lane).not.toHaveClass(/\boff\b/);
    expect(calls.sent('timeline.set').at(-1).timeline.lanes[0].on).not.toBe(false);

    const play = panel.locator('.seq-play');
    await play.click();
    await expect(play).toHaveText(/Stop/, { timeout: 250 });
    await play.click();
    await expect(play).toHaveText(/Play/, { timeout: 250 });
    await page.waitForTimeout(1000);
    expect(calls.sent('timeline.play')).toHaveLength(1);
    expect(calls.sent('timeline.stop')).toHaveLength(1);
    await expect(play).toHaveText(/Play/);
  });

  // A trigger can't watch a channel the sequence's own lanes drive: the engine refuses
  // it, so the editor doesn't offer it.
  test('a trigger is never offered a channel its own lanes drive', async ({ page, daemon }) => {
    await connect(page, daemon);
    const panel = await sequenceWithLane(page, 'Glow');
    await panel.getByRole('button', { name: '+ Trigger' }).click();
    const editor = page.getByRole('dialog', { name: 'New trigger' });
    await expect(editor.getByRole('combobox', { name: 'Which' })).toHaveText('None yet');
    await editor.getByRole('button', { name: 'Cancel' }).click();
    // Another channel is offered.
    await panel.getByRole('button', { name: /Add lane/ }).click();
    await page.getByRole('searchbox', { name: 'Search channels and settings' }).fill('Dim');
    await page.getByRole('button', { name: '+ New channel “Dim”' }).click();
    await panel.locator('.seq-lane:not(.seq-events)').nth(1).getByRole('button', { name: 'Remove this lane' }).click();
    await expect(panel.locator('.seq-lane-name', { hasText: 'Dim' })).toHaveCount(0);
    await panel.getByRole('button', { name: '+ Trigger' }).click();
    await expect(editor.getByRole('combobox', { name: 'Which' }).locator('option')).toHaveText(['Dim']);
    await editor.getByRole('button', { name: 'Play it' }).click();
    await expect(panel.locator('.seq-chip')).toHaveText([/Dim rises/]);
  });
});
