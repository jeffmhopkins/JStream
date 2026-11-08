// Wheel chords in the page (ADR-0112 §3, #1346): with Ctrl+WheelUp and Ctrl+WheelDown
// bound to the follow camera's zoom, each notch runs the action once, whether Chromium
// reports a notch as 120 pixels (Linux, at every devicePixelRatio) or 100 (Windows);
// a touchpad's small deltas add up to a notch; a Shift turn reported sideways counts;
// and a turn with no bound chord keeps the browser's own use of the wheel.
//
// Playwright's wheel sends the deltas it is given, so this pins how the page counts
// them, not what a real wheel reports: that was measured with real X input (#1346). The
// devicePixelRatio is the browser's own (`--force-device-scale-factor`), not emulated:
// emulating one scales the deltas the page sees, which no real display does.
import { test, expect, connect } from '../fixtures.js';

/** Routes the page's WebSocket, passing everything on, and returns `zooms()`, the
 * `scene.zoom_camera` actions the page sent, and `bind(chord, steps)`, which binds a
 * chord to zoom Main from outside the page, as another client would. */
async function route(page) {
  const zooms = [];
  let server = null;
  let next = 0;
  await page.routeWebSocket(/.*/, (ws) => {
    server = ws.connectToServer();
    ws.onMessage((message) => {
      try {
        const parsed = JSON.parse(message);
        if (parsed.call?.kind === 'action' && parsed.call.action?.type === 'scene.zoom_camera') {
          zooms.push(parsed.call.action.steps);
        }
      } catch {
        // Not JSON: pass it on.
      }
      server.send(message);
    });
    server.onMessage((message) => {
      // The answers to this route's own requests aren't the page's.
      if (typeof message === 'string' && message.includes('"id":"e2e-wheel-')) return;
      ws.send(message);
    });
  });
  const bind = (chord, steps) => {
    next += 1;
    server.send(JSON.stringify({
      type: 'request',
      id: `e2e-wheel-${next}`,
      call: {
        kind: 'batch',
        edits: [{ type: 'hotkey.set', chord, action: { type: 'scene.zoom_camera', scene_id: 'main', steps } }],
      },
    }));
  };
  return { zooms: () => zooms.slice(), bind };
}

/** Counts, from here on, the wheel events that reached the window with the browser's
 * own use of them prevented, and those without. */
async function watchDefaults(page) {
  await page.evaluate(() => {
    window.wheelDefaults = { prevented: 0, kept: 0 };
    addEventListener('wheel', (e) => {
      window.wheelDefaults[e.defaultPrevented ? 'prevented' : 'kept'] += 1;
    }, { passive: true });
  });
  return () => page.evaluate(() => ({ ...window.wheelDefaults }));
}

/** A page in a browser of its own at `scale`, the browser's own devicePixelRatio, signed
 * in to `daemon`. Its console errors fail the test, as the fixture's do, apart from the
 * mock's refusal of each zoom (it has no follow camera, as the engine refuses a scene
 * without one). */
async function pageAt(playwright, daemon, scale) {
  const browser = await playwright.chromium.launch({
    executablePath: process.env.JSTREAM_E2E_CHROMIUM || undefined,
    args: [`--force-device-scale-factor=${scale}`, '--window-size=1280,800'],
  });
  // The project's device sets an emulated scale and viewport: neither, here.
  const context = await browser.newContext({
    viewport: null,
    deviceScaleFactor: undefined,
    isMobile: undefined,
    hasTouch: undefined,
    httpCredentials: { username: 'jstream', password: daemon.token },
  });
  const page = await context.newPage();
  const errors = [];
  page.on('console', (m) => {
    if (m.type() === 'error' && !/follows the pointer/.test(m.text())) errors.push(m.text());
  });
  page.on('pageerror', (e) => errors.push(String(e)));
  return {
    page,
    close: async () => {
      await browser.close();
      expect(errors, 'console errors on the page').toEqual([]);
    },
  };
}

for (const deviceScaleFactor of [1, 2]) {
  test(`a bound wheel chord runs once per notch, at a devicePixelRatio of ${deviceScaleFactor}`, async ({ playwright, daemon }) => {
    const { page, close } = await pageAt(playwright, daemon, deviceScaleFactor);
    try {
      const calls = await route(page);
      await connect(page, daemon);
      expect(await page.evaluate(() => devicePixelRatio)).toBe(deviceScaleFactor);
      calls.bind('Ctrl+WheelUp', 1);
      calls.bind('Ctrl+WheelDown', -1);
      const stage = page.locator('.preview.stage');
      const box = await stage.boundingBox();
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);

      // The bindings reach the page a little later: turn until a notch is the chord's,
      // and every notch that was has run.
      const defaults = await watchDefaults(page);
      await page.keyboard.down('Control');
      await expect.poll(async () => {
        await page.mouse.wheel(0, -120);
        return (await defaults()).prevented;
      }).toBeGreaterThan(0);
      const warm = await defaults();
      await expect.poll(() => calls.zooms().length).toBe(warm.prevented);
      const start = calls.zooms().length;

      // Chromium's notch on Linux, then on Windows: one step each.
      await page.mouse.wheel(0, -120);
      await page.mouse.wheel(0, -100);
      await page.mouse.wheel(0, 120);
      await page.mouse.wheel(0, 100);
      await expect.poll(() => calls.zooms().slice(start)).toEqual([1, 1, -1, -1]);

      // Two notches coalesced into one event are two steps.
      await page.mouse.wheel(0, -240);
      await expect.poll(() => calls.zooms().slice(start)).toEqual([1, 1, -1, -1, 1, 1]);

      // A touchpad's small deltas add up to one notch.
      for (let i = 0; i < 6; i += 1) await page.mouse.wheel(0, 20);
      await expect.poll(() => calls.zooms().slice(start)).toEqual([1, 1, -1, -1, 1, 1, -1]);
      await page.keyboard.up('Control');

      // Every Ctrl turn since was the chord's: the page didn't zoom.
      expect((await defaults()).kept).toBe(warm.kept);

      // With no chord bound for it, a turn keeps the browser's own use and runs nothing.
      const before = calls.zooms().length;
      await page.mouse.wheel(0, 120);
      await expect.poll(async () => (await defaults()).kept).toBe(warm.kept + 1);
      expect(calls.zooms().length).toBe(before);
    } finally {
      await close();
    }
  });
}

test('a Shift wheel chord counts the turn browsers report sideways', async ({ page, daemon, consoleErrors }) => {
  consoleErrors.allow(/follows the pointer/);
  const calls = await route(page);
  await connect(page, daemon);
  const defaults = await watchDefaults(page);
  calls.bind('Shift+WheelDown', -1);
  const box = await page.locator('.preview.stage').boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.keyboard.down('Shift');
  // Chromium and Firefox put a Shift turn in deltaX.
  await expect.poll(async () => {
    await page.mouse.wheel(120, 0);
    return (await defaults()).prevented;
  }).toBeGreaterThan(0);
  const warm = await defaults();
  await expect.poll(() => calls.zooms().length).toBe(warm.prevented);
  const start = calls.zooms().length;
  await page.mouse.wheel(120, 0);
  await expect.poll(() => calls.zooms().slice(start)).toEqual([-1]);
  // Shift+WheelUp isn't bound: that turn scrolls sideways as usual.
  const kept = (await defaults()).kept;
  await page.mouse.wheel(-120, 0);
  await expect.poll(async () => (await defaults()).kept).toBe(kept + 1);
  await page.keyboard.up('Shift');
  expect(calls.zooms().slice(start)).toEqual([-1]);
});
