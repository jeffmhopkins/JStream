// The Show panel's labels on a crowded timeline: keyframes, events and cut badges close
// together don't draw over each other. A marker's name stops short of the next marker
// or is left out, a stack of markers is named once with "+N", a cut badge keeps to
// half the gap each side, and a clip's name stays inside its clip. Hovering a marker
// shows its whole name; every name is in a tooltip.
import { test, expect, connect } from '../fixtures.js';

/** The rectangles of `locator`'s visible elements. */
async function boxes(locator) {
  return locator.evaluateAll((els) =>
    els
      .filter((el) => el.offsetParent !== null && el.getBoundingClientRect().width > 0)
      .map((el) => {
        const r = el.getBoundingClientRect();
        return { left: r.left, right: r.right, text: el.textContent };
      })
      .sort((a, b) => a.left - b.left),
  );
}

/** Checks that no two of `rects` overlap along the row. */
function noOverlap(rects, what) {
  for (let n = 1; n < rects.length; n += 1) {
    expect(rects[n].left, `${what}: "${rects[n - 1].text}" runs into "${rects[n].text}"`).toBeGreaterThanOrEqual(
      rects[n - 1].right - 0.5,
    );
  }
}

test('labels on a crowded show stay clear of each other', async ({ page, daemon }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  let server;
  const requests = [];
  const replies = [];
  await page.routeWebSocket(/.*/, (ws) => {
    server = ws.connectToServer();
    ws.onMessage((message) => {
      requests.push(String(message));
      server.send(message);
    });
    server.onMessage((message) => {
      replies.push(String(message));
      ws.send(message);
    });
  });
  await connect(page, daemon);
  for (const name of ['Show', 'Audio mixer', 'Outputs', 'Transitions']) {
    await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
    await page.getByRole('menu', { name: 'View' }).getByRole('menuitemcheckbox', { name, exact: true }).click();
    await page.keyboard.press('Escape');
  }
  const splitter = await page.getByRole('separator', { name: 'Resize bottom dock' }).boundingBox();
  await page.mouse.move(splitter.x + splitter.width / 2, splitter.y + splitter.height / 2);
  await page.mouse.down();
  await page.mouse.move(splitter.x + splitter.width / 2, splitter.y - 330, { steps: 6 });
  await page.mouse.up();
  const panel = page.locator('.show-panel');
  await panel.getByRole('button', { name: '+ New show' }).click();
  await panel.getByRole('textbox', { name: 'Show name' }).press('Enter');
  await expect(panel.locator('.show-track-program')).toBeVisible();
  const made = requests.map((r) => JSON.parse(r)).findLast((r) => JSON.stringify(r).includes('"show.set"'));
  const base = made.call.edits[0].show;
  // The seed's scenes, as the engine reported them.
  const scenes = [...new Set(replies.join('').match(/"scene_id":"[^"]+"/g).map((m) => m.slice(12, -1)))];
  expect(scenes.length).toBeGreaterThan(1);

  // Short program clips with a cut badge each, keyframes in stacks and pairs, events
  // close together, and an overlay track of thin clips.
  const lengths = [4000, 600, 3000, 3000, 400, 2500, 3000, 5000];
  const show = {
    ...base,
    program: lengths.map((length_ms, n) => ({
      clip_id: `p${n}`,
      scene_id: scenes[n % 2],
      length_ms,
    })),
    overlays: [
      {
        track_id: 'v2',
        name: 'Chapter labels',
        clips: [500, 4100, 4300, 9000].map((start_ms, n) => ({
          clip_id: `o${n}`,
          scene_id: scenes[1],
          start_ms,
          length_ms: 150,
        })),
      },
    ],
    keyframes: [1000, 1050, 1100, 1150, 2500, 2700, 6000, 6300, 6350, 9000, 12000].map((at_ms, n) => ({
      marker_id: `k${n}`,
      at_ms,
    })),
    sequences: [
      {
        track_id: 's1',
        name: 'Events',
        cues: [3000, 3080, 3400, 8000, 11000].map((at_ms, n) => ({
          cue_id: `c${n}`,
          at_ms,
          action: { type: 'source.restart', source_id: `clip-${n}` },
        })),
      },
    ],
  };
  server.send(JSON.stringify({ type: 'request', id: 'req_labels', call: { kind: 'batch', edits: [{ type: 'show.set', show }] } }));
  await expect(panel.locator('.show-kf')).toHaveCount(11);

  if (process.env.JSTREAM_E2E_SHOTS) {
    await page.mouse.move(0, 0);
    await panel.screenshot({ path: `${process.env.JSTREAM_E2E_SHOTS}/show-labels.png` });
  }

  // Keyframes: the stack of four is named once, with "+3", and lists every name.
  const stack = panel.locator('.show-kf').first();
  await expect(stack.locator('.show-mk-more')).toHaveText('+3');
  expect((await stack.getAttribute('title')).split('\n')).toHaveLength(4);
  // Stacked diamonds overlap by design; names and counts stop short of the next diamond.
  const names = await boxes(panel.locator('.show-kf .show-mk-name, .show-kf .show-mk-more'));
  const diamonds = await boxes(panel.locator('.show-kf s'));
  noOverlap(names, 'keyframe names');
  for (const name of names) {
    const next = diamonds.find((d) => d.left > name.left);
    if (next) expect(name.right, `"${name.text}" runs into the next diamond`).toBeLessThanOrEqual(next.left);
  }
  // Events: a pill with its name ends before the next pill; stacked bare pills overlap.
  const named = await boxes(panel.locator('.show-ev span:not(.bare)'));
  const pills = await boxes(panel.locator('.show-ev span'));
  for (const pill of named) {
    const next = pills.find((p) => p.left > pill.left);
    if (next) expect(pill.right, `"${pill.text}" runs into the next event`).toBeLessThanOrEqual(next.left);
  }
  // Cut badges: one with its label clears its neighbours; bare icons may touch.
  const cuts = await boxes(panel.locator('.show-cut'));
  const labelled = await boxes(panel.locator('.show-cut:not(.bare)'));
  expect(labelled.length).toBeGreaterThan(0);
  for (const cut of labelled) {
    for (const other of cuts.filter((c) => c.left !== cut.left)) {
      const clear = other.right <= cut.left + 0.5 || other.left >= cut.right - 0.5;
      expect(clear, `cut badge "${cut.text}" runs into "${other.text}"`).toBe(true);
    }
  }

  // A clip's name stays inside its clip.
  for (const clip of await panel.locator('.show-clip').all()) {
    const c = await clip.boundingBox();
    const name = clip.locator('.show-clip-name');
    if (await name.isVisible()) {
      const n = await name.boundingBox();
      expect(n.x + n.width).toBeLessThanOrEqual(c.x + c.width + 0.5);
    }
  }

  // Hovering a keyframe whose name was left out shows it whole.
  const hidden = panel.locator('.show-kf:has(.show-mk-name.hide)').first();
  await hidden.hover();
  await expect(hidden.locator('.show-mk-name')).toBeVisible();

});
