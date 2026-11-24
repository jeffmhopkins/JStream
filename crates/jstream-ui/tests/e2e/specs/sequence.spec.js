// The Sequence panel against the mock engine, which stores and plays timelines but
// keeps no clock (jstream-control's README): making a sequence, its lanes and points,
// Play and Stop, clicks that come before the engine has answered the last one, and the
// on/off gestures: a switch lane's bars and the Events lane's chips.
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
    await panel.getByRole('button', { name: 'Add a trigger that starts it' }).click();
    const editor = page.getByRole('dialog', { name: 'New trigger' });
    await expect(editor.getByRole('combobox', { name: 'Which' })).toHaveText('None yet');
    await editor.getByRole('button', { name: 'Cancel' }).click();
    // Another channel is offered.
    await panel.getByRole('button', { name: /Add lane/ }).click();
    await page.getByRole('searchbox', { name: 'Search channels and settings' }).fill('Dim');
    await page.getByRole('button', { name: '+ New channel “Dim”' }).click();
    await panel.locator('.seq-lane:not(.seq-events)').nth(1).getByRole('button', { name: 'Remove this lane' }).click();
    await expect(panel.locator('.seq-lane-name', { hasText: 'Dim' })).toHaveCount(0);
    await panel.getByRole('button', { name: 'Add a trigger that starts it' }).click();
    await expect(editor.getByRole('combobox', { name: 'Which' }).locator('option')).toHaveText(['Dim']);
    await editor.getByRole('button', { name: 'Add trigger' }).click();
    await expect(panel.locator('.seq-chip')).toHaveText([/Dim rises/]);
  });

  // A switch lane's bars (docs/UI-FEATURES.md SQ-02): a double-click in a gap adds one,
  // its body moves it, its ends change it, a click or a right-click opens its menu, and
  // Delete removes it. Each gesture is one timeline.set, and the lane stays a switch.
  test('a switch lane bar is added, moved, stretched and deleted by hand', async ({ page, daemon }) => {
    const calls = await slowEngine(page, 0);
    await connect(page, daemon);
    const panel = await sequenceWithLane(page, 'Lamp');
    const lane = panel.locator('.seq-lane:not(.seq-events)').first();
    // The mock's scene has no on/off settings to offer: make the lane a switch by
    // hand, its one point held at 0. A click on a point opens its menu.
    await lane.locator('.seq-dot').click();
    const pointMenu = page.getByRole('menu', { name: 'Point' });
    await pointMenu.getByRole('textbox', { name: 'Point value' }).fill('0');
    await pointMenu.getByRole('textbox', { name: 'Point value' }).press('Enter');
    await lane.locator('.seq-dot').click();
    await pointMenu.getByRole('menuitem', { name: 'Hold, then jump' }).click();
    await expect(lane.locator('.seq-lane-meta')).toContainText('Switch');
    await expect(lane.locator('.seq-dot')).toHaveCount(0);
    const bars = lane.locator('.seq-switch-bar');
    await expect(bars).toHaveCount(0);

    const area = await lane.locator('.seq-area').boundingBox();
    const x = (ms) => area.x + (area.width * ms) / 6000;
    const y = area.y + area.height / 2;
    // A double-click in a gap adds a one-second bar there.
    await page.mouse.dblclick(x(1000), y);
    await expect(bars).toHaveCount(1);
    await expect(bars.first()).toHaveAccessibleName('On from 1 s to 2 s');
    // In a bar, it adds nothing.
    let sets = calls.sent('timeline.set').length;
    await page.mouse.dblclick(x(1500), y);
    await page.waitForTimeout(300);
    expect(calls.sent('timeline.set')).toHaveLength(sets);
    await expect(bars).toHaveCount(1);

    // Its body moves it, keeping its length, as one edit.
    const drag = async (from, to) => {
      await page.mouse.move(from, y);
      await page.mouse.down();
      await page.mouse.move(to, y, { steps: 6 });
      await page.mouse.up();
    };
    sets = calls.sent('timeline.set').length;
    await drag(x(1500), x(3500));
    await expect(bars.first()).toHaveAccessibleName('On from 3 s to 4 s');
    expect(calls.sent('timeline.set')).toHaveLength(sets + 1);
    // Its ends change where it switches on and off.
    const box = await bars.first().boundingBox();
    await drag(box.x + box.width - 1, x(5000));
    await expect(bars.first()).toHaveAccessibleName('On from 3 s to 5 s');
    await drag(box.x + 1, x(2000));
    await expect(bars.first()).toHaveAccessibleName('On from 2 s to 5 s');
    // Dragged past the end, it stops there.
    await drag(x(3000), x(5900));
    await expect(bars.first()).toHaveAccessibleName('On from 3 s to 6 s');
    // What was sent: off from the start, on at 3 s, held on to the end.
    const points = calls.sent('timeline.set').at(-1).timeline.lanes[0].points;
    expect(points.map((p) => [p.at_ms, p.value, p.curve])).toEqual([[0, 0, 'hold'], [3000, 1, 'hold']]);

    // A second bar stops a tenth of a second short of the first: two bars never join.
    await page.mouse.dblclick(x(500), y);
    await expect(bars).toHaveCount(2);
    await drag(x(1000), x(4000));
    await expect(bars.first()).toHaveAccessibleName('On from 1.9 s to 2.9 s');

    // A click opens its menu; typed times are kept between its neighbours.
    await bars.first().click();
    const barMenu = page.getByRole('menu', { name: 'Bar' });
    await expect(barMenu.getByRole('menuitem')).toHaveText(['Delete bar']);
    await barMenu.getByRole('textbox', { name: 'Switches off at' }).fill('9');
    await barMenu.getByRole('textbox', { name: 'Switches off at' }).press('Enter');
    await expect(bars.first()).toHaveAccessibleName('On from 1.9 s to 2.9 s');
    await bars.first().click();
    await barMenu.getByRole('textbox', { name: 'Switches on at' }).fill('0.5');
    await barMenu.getByRole('textbox', { name: 'Switches on at' }).press('Enter');
    await expect(bars.first()).toHaveAccessibleName('On from 0.5 s to 2.9 s');
    // A right-click opens it too, and deletes from it.
    await bars.first().click({ button: 'right' });
    await barMenu.getByRole('menuitem', { name: 'Delete bar' }).click();
    await expect(bars).toHaveCount(1);
    // Delete removes a focused bar; the lane stays a switch with none.
    await bars.first().focus();
    await page.keyboard.press('Delete');
    await expect(bars).toHaveCount(0);
    await expect(lane.locator('.seq-lane-meta')).toContainText('Switch');
  });

  // An on/off event (docs/UI-FEATURES.md SQ-04) answers the same gestures as a point:
  // a click or a right-click opens it, a drag moves it, Delete removes it.
  test('an on/off event opens on a click or a right-click and moves on a drag', async ({ page, daemon }) => {
    await connect(page, daemon);
    const panel = await sequenceWithLane(page, 'Lamp');
    const events = panel.locator('.seq-events-area');
    const area = await events.boundingBox();
    const x = (ms) => area.x + (area.width * ms) / 6000;
    const y = area.y + area.height / 2;
    await page.mouse.dblclick(x(1000), y);
    const editor = page.getByRole('dialog', { name: 'Event' });
    await editor.getByRole('combobox').first().selectOption({ label: 'Switch a channel on' });
    await editor.getByRole('button', { name: /Add/ }).click();
    const chip = panel.locator('.seq-event');
    await expect(chip).toHaveText([/Lamp/]);
    await chip.click({ button: 'right' });
    await expect(editor).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(editor).toHaveCount(0);
    await chip.click();
    await expect(editor).toBeVisible();
    await page.keyboard.press('Escape');
    const box = await chip.boundingBox();
    await page.mouse.move(box.x + 4, y);
    await page.mouse.down();
    await page.mouse.move(x(3000) + 4, y, { steps: 6 });
    await page.mouse.up();
    await expect(editor).toHaveCount(0);
    await expect(chip).toHaveAttribute('title', /at 3 s/);
    await chip.focus();
    await page.keyboard.press('Delete');
    await expect(chip).toHaveCount(0);
  });

  // The zoom bar along the foot, as the Show panel's: + zooms in from the keyboard, the
  // ruler's ticks get finer, a double-click on a zoomed lane still lands where it was
  // clicked, Ctrl+wheel zooms and a double-click on the bar shows the whole sequence.
  test('the zoom bar zooms and scrolls the lanes', async ({ page, daemon }) => {
    await connect(page, daemon);
    const panel = await sequenceWithLane(page, 'Haze');
    const bar = panel.getByRole('scrollbar', { name: 'The part of the sequence in view' });
    await expect(panel.locator('.seq-zoom-label')).toHaveText('Whole sequence · 6 s');
    const clip = panel.locator('.seq-ruler-row .seq-clip');
    const ruler = panel.locator('.seq-ruler');
    const width = async (l) => (await l.boundingBox()).width;
    expect(Math.abs((await width(ruler)) - (await width(clip)) + 16)).toBeLessThan(1);
    await expect(ruler.locator('.seq-tick', { hasText: /^0\.2 s$/ })).toHaveCount(0);

    // Zoomed in about four times, at the start.
    await bar.focus();
    for (let n = 0; n < 6; n += 1) await page.keyboard.press('+');
    await page.keyboard.press('Home');
    expect(await width(ruler)).toBeGreaterThan(3 * (await width(clip)));
    await expect(ruler.locator('.seq-tick', { hasText: /^0\.2 s$/ })).toHaveCount(1);
    const thumb = await panel.locator('.show-thumb').boundingBox();
    expect(thumb.width).toBeLessThan((await bar.boundingBox()).width / 3);

    // A double-click at half a second, on the zoomed lane, adds a point there.
    const lane = panel.locator('.seq-lane:not(.seq-events)').first();
    const area = await lane.locator('.seq-area').boundingBox();
    await page.mouse.dblclick(area.x + (area.width * 500) / 6000, area.y + area.height / 2);
    await expect(lane.locator('.seq-dot')).toHaveCount(2);
    await expect(lane.getByRole('button', { name: /^0\.5 s · / })).toHaveCount(1);

    // A double-click on the bar shows it all again; Ctrl+wheel over the lanes zooms in.
    await bar.dblclick();
    expect(Math.abs((await width(ruler)) - (await width(clip)) + 16)).toBeLessThan(1);
    const c = await clip.boundingBox();
    await page.mouse.move(c.x + c.width / 2, area.y + area.height / 2);
    await page.keyboard.down('Control');
    await page.mouse.wheel(0, -400);
    await page.keyboard.up('Control');
    await expect.poll(() => width(ruler)).toBeGreaterThan(1.5 * c.width);

    if (process.env.JSTREAM_E2E_SHOTS) {
      await panel.screenshot({ path: `${process.env.JSTREAM_E2E_SHOTS}/sequence-zoom.png` });
    }
  });
});
