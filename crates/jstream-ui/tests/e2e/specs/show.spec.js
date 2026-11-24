// The Show panel's Edit view, driven as a person would: a new show from the empty
// panel, named in place; the seed's scenes Intro and Main dragged from the Scenes panel
// onto V1, each ending in a pausing bookmark; a bookmark placed with the Bookmark tool,
// a loop drawn along the ruler with the Loop tool, Main trimmed by its right end; and
// Edit › Undo taking back one gesture at a time. Each gesture is one `show.set`.
import { test, expect, connect, recordCalls } from '../fixtures.js';

function panel(page) {
  return page.locator('.show-panel');
}

/** The centre of `locator` on screen. */
async function centre(locator) {
  const box = await locator.boundingBox();
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Drags scene `name` from the Scenes panel to (`x`, `y`) and lets go there, checking on
 * the way that the drop slot at the end of V1 shows. */
async function dragScene(page, name, x, y) {
  const row = page.locator('.scenes-panel li.row .row-main').filter({ hasText: new RegExp(`^${name}`) });
  const from = await centre(row.first());
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + 10, from.y + 10, { steps: 3 });
  await page.mouse.move(x, y, { steps: 12 });
  await expect(panel(page).locator('.show-slot')).toBeVisible();
  await page.mouse.up();
  await expect(panel(page).locator('.show-slot')).toHaveCount(0);
}

async function undo(page) {
  await page.locator('header.app-header .top-menu[data-menu="edit"] > .menu-button').click();
  await page.getByRole('menu', { name: 'Edit' }).getByRole('menuitem', { name: 'Undo' }).click();
}

/** Shows the Show panel alone along the bottom dock, made tall. */
async function openShowPanel(page) {
  // The Show panel alone along the bottom, made tall.
  for (const name of ['Show', 'Audio mixer', 'Outputs', 'Transitions']) {
    await page.locator('header.app-header').getByRole('button', { name: 'View' }).click();
    await page.getByRole('menu', { name: 'View' }).getByRole('menuitemcheckbox', { name, exact: true }).click();
    await page.keyboard.press('Escape');
  }
  const splitter = await centre(page.getByRole('separator', { name: 'Resize bottom dock' }));
  await page.mouse.move(splitter.x, splitter.y);
  await page.mouse.down();
  await page.mouse.move(splitter.x, splitter.y - 330, { steps: 6 });
  await page.mouse.up();
}

/** Makes a show from the empty panel and drags Intro and Main onto V1. */
async function introAndMain(page) {
  const show = panel(page);
  await show.getByRole('button', { name: '+ New show' }).click();
  await show.getByRole('textbox', { name: 'Show name' }).press('Enter');
  const v1 = show.locator('.show-track-program .show-track-body');
  let box = await v1.boundingBox();
  await dragScene(page, 'Intro', box.x + 40, box.y + box.height / 2);
  await expect(show.locator('.show-prog')).toHaveCount(1);
  box = await v1.boundingBox();
  await dragScene(page, 'Main', box.x + box.width - 20, box.y + box.height / 2);
  await expect(show.locator('.show-prog b')).toHaveText(['Intro', 'Main']);
}

/** Drops files at `paths` on (`x`, `y`) as a file manager does in a browser: a drag
 * whose data is a `text/uri-list`. */
async function dropFiles(page, paths, x, y) {
  await page.evaluate(([paths, x, y]) => {
    const data = new DataTransfer();
    data.setData('text/uri-list', paths.map((p) => `file://${p}`).join('\r\n'));
    const target = document.elementFromPoint(x, y);
    for (const type of ['dragover', 'drop']) {
      target.dispatchEvent(new DragEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y, dataTransfer: data }));
    }
  }, [paths, x, y]);
}

test('a show is built on the timeline, one undo step per gesture', async ({ page, daemon }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await openShowPanel(page);
  const show = panel(page);
  const sets = () => calls.sent('show.set').length;

  // An empty collection offers a new show, named in place.
  await expect(show.getByText('No show yet')).toBeVisible();
  await show.getByRole('button', { name: '+ New show' }).click();
  const name = show.getByRole('textbox', { name: 'Show name' });
  await expect(name).toBeFocused();
  await name.fill('Friday show');
  await name.press('Enter');
  await expect(show.getByRole('combobox', { name: 'Show' }).locator('option:checked')).toHaveText('Friday show');
  await expect(show.getByText('Audio tracks are optional.', { exact: false })).toBeVisible();

  // Intro and Main dragged onto V1: Main goes after Intro, and each ends in a pause.
  const v1 = show.locator('.show-track-program .show-track-body');
  let box = await v1.boundingBox();
  await dragScene(page, 'Intro', box.x + 40, box.y + box.height / 2);
  await expect(show.locator('.show-prog')).toHaveCount(1);
  const slot = show.locator('.show-slot');
  box = await v1.boundingBox();
  await dragScene(page, 'Main', box.x + box.width - 20, box.y + box.height / 2);
  await expect(show.locator('.show-prog')).toHaveCount(2);
  await expect(show.locator('.show-prog b')).toHaveText(['Intro', 'Main']);
  await expect(show.locator('.show-flag')).toHaveText([/Intro/, /Main/]);
  await expect(show.locator('.show-flag.show-sel')).toHaveCount(0);
  await expect(show.locator('.show-prog span')).toHaveText(['0:30', '0:30']);
  const afterDrops = sets();

  // Stopped, "Next:" names what Play brings in from the play head, and a click on it
  // skips the play head ahead there.
  const next = show.locator('.show-next');
  await expect(next).toHaveText('Next: scene Intro');
  const rulerBox = await show.locator('.show-ruler').boundingBox();
  const introBox = await show.locator('.show-prog').first().boundingBox();
  await page.mouse.click(introBox.x + introBox.width / 2, rulerBox.y + rulerBox.height / 2);
  await expect(next).toHaveText('Next: scene Main');
  await next.click();
  await expect(show.locator('.show-head em')).toHaveText('0:30 · Play starts here');
  // On Main's start, Next is what starts after it: nothing does, so it goes away rather
  // than pointing back at Main.
  await expect(next).toHaveCount(0);
  expect(sets()).toBe(afterDrops);

  // The Bookmark tool (B): a click in Intro's middle adds a pausing bookmark there.
  await show.locator('.show-prog').first().click();
  await expect(show.locator('.show-prog').first()).toHaveClass(/show-sel/);
  await page.keyboard.press('b');
  await expect(show.getByRole('button', { name: 'Bookmark (B)' })).toHaveAttribute('aria-pressed', 'true');
  await show.locator('.show-prog').first().click();
  await expect(show.locator('.show-flag')).toHaveCount(3);
  await expect(show.locator('.show-flag').nth(0)).toHaveText(/Bookmark 3/);
  expect(sets()).toBe(afterDrops + 1);

  // The Loop tool (L): a drag along the ruler over Intro makes a loop.
  await page.keyboard.press('l');
  const ruler = show.locator('.show-ruler');
  const intro = await show.locator('.show-prog').first().boundingBox();
  const r = await ruler.boundingBox();
  await page.mouse.move(intro.x + 2, r.y + r.height / 2);
  await page.mouse.down();
  await page.mouse.move(intro.x + intro.width * 0.6, r.y + r.height / 2, { steps: 8 });
  await expect(show.locator('.show-loop em')).toBeVisible();
  await page.mouse.up();
  await expect(show.locator('.show-loop em')).toContainText('Loop 1');
  expect(sets()).toBe(afterDrops + 2);

  // Select (V): Main's right end dragged left trims it, and its pause moves with it.
  await page.keyboard.press('v');
  const main = show.locator('.show-prog').nth(1);
  const m = await main.boundingBox();
  await page.mouse.move(m.x + m.width - 3, m.y + m.height / 2);
  await page.mouse.down();
  await page.mouse.move(m.x + m.width / 2, m.y + m.height / 2, { steps: 8 });
  await expect(show.locator('.show-tip')).toContainText('drag to trim');
  await page.mouse.up();
  await expect(show.locator('.show-prog span').nth(1)).not.toHaveText('0:30');
  expect(sets()).toBe(afterDrops + 3);

  // Each undo takes back one gesture: the trim, then the loop.
  await undo(page);
  await expect(show.locator('.show-prog span')).toHaveText(['0:30', '0:30']);
  await expect(show.locator('.show-loop em')).toHaveCount(1);
  await undo(page);
  await expect(show.locator('.show-loop em')).toHaveCount(0);
  await expect(show.locator('.show-flag')).toHaveCount(3);
  void slot;
});

test('cuts, audio tracks, media drops, deletes, the lock and the show menu', async ({ page, daemon }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  const calls = await recordCalls(page);
  await connect(page, daemon);
  await openShowPanel(page);
  await introAndMain(page);
  const show = panel(page);
  const menu = page.getByRole('menu', { name: 'Context menu' });

  // The cut badge opens the cut's transition menu: Default is the one in use.
  await show.locator('.show-cut').click();
  await expect(menu.getByRole('menuitemcheckbox', { name: 'Default' })).toBeVisible();
  await page.keyboard.press('Escape');

  // An audio track, and an audio clip on it from its menu, with default fades.
  await show.getByRole('button', { name: '+ Audio track' }).click();
  const a1 = show.locator('.show-track-audio .show-track-body');
  await expect(a1).toHaveCount(1);
  const a = await a1.boundingBox();
  await page.mouse.click(a.x + 30, a.y + a.height / 2, { button: 'right' });
  await menu.getByRole('menuitem', { name: /^Add source audio at / }).hover();
  await page.getByRole('menuitem', { name: 'Mic · ES-9 Line' }).click();
  await expect(show.locator('.show-au')).toHaveCount(1);
  await expect(show.locator('.show-au span')).toHaveText('fade in 0.5 s · fade out 0.5 s');

  // Selected, Delete removes it, in one show.set.
  const before = calls.sent('show.set').length;
  await show.locator('.show-au').click();
  await page.keyboard.press('Delete');
  await expect(show.locator('.show-au')).toHaveCount(0);
  expect(calls.sent('show.set').length).toBe(before + 1);

  // A video file dropped after Main makes its source and scene in the same batch as the
  // show, and goes in as a timed clip with no pause at its end.
  const v1 = await show.locator('.show-track-program .show-track-body').boundingBox();
  await dropFiles(page, ['/media/clips/intro.mp4'], v1.x + v1.width - 10, v1.y + v1.height / 2);
  await expect(show.locator('.show-prog b')).toHaveText(['Intro', 'Main', 'intro.mp4']);
  await expect(show.locator('.show-prog .show-timed')).toHaveCount(1);
  await expect(show.locator('.show-flag')).toHaveCount(2);
  const batch = calls.sent('show.set').at(-1);
  expect(batch.show.program).toHaveLength(3);
  expect(calls.sent('source.create').length).toBe(1);
  expect(calls.sent('scene.create').length).toBe(1);

  // Locked, the tools are off and Delete leaves the clip.
  await show.getByRole('button', { name: '🔓 Lock' }).click();
  // On, Lock takes the show accent, so a locked show stands out.
  await expect(show.getByRole('button', { name: '🔒 Locked' })).toHaveCSS('color', 'rgb(245, 209, 66)');
  await expect(show.getByRole('button', { name: 'Split (S)' })).toBeDisabled();
  await show.locator('.show-prog').first().click();
  await page.keyboard.press('Delete');
  await expect(show.locator('.show-prog')).toHaveCount(3);
  await show.getByRole('button', { name: '🔒 Locked' }).click();

  // A loop over Intro, then a trim of Main held mid-drag, with its tip: Escape lets go
  // and sends nothing.
  await page.keyboard.press('l');
  const ruler = await show.locator('.show-ruler').boundingBox();
  const intro = await show.locator('.show-prog').first().boundingBox();
  await page.mouse.move(intro.x + 2, ruler.y + ruler.height / 2);
  await page.mouse.down();
  await page.mouse.move(intro.x + intro.width - 2, ruler.y + ruler.height / 2, { steps: 8 });
  await page.mouse.up();
  await expect(show.locator('.show-loop em')).toContainText('Loop 1');
  await page.keyboard.press('v');
  const sent = calls.sent('show.set').length;
  const main = await show.locator('.show-prog').nth(1).boundingBox();
  await page.mouse.move(main.x + main.width - 3, main.y + main.height / 2);
  await page.mouse.down();
  await page.mouse.move(main.x + main.width * 0.75, main.y + main.height / 2, { steps: 8 });
  await expect(show.locator('.show-tip')).toContainText('drag to trim');
  await page.screenshot({ path: process.env.SHOW_SHOT || 'test-results/show-edit.png' });
  await page.keyboard.press('Escape');
  await page.mouse.up();
  await expect(show.locator('.show-tip')).toHaveCount(0);
  await expect(show.locator('.show-prog span').nth(1)).toHaveText('0:30');
  expect(calls.sent('show.set').length).toBe(sent);

  // The show menu: Duplicate opens a copy, Rename renames it, Delete takes it away.
  const picker = show.getByRole('combobox', { name: 'Show' });
  await show.getByRole('button', { name: 'Show menu' }).click();
  await menu.getByRole('menuitem', { name: 'Duplicate' }).click();
  await expect(picker.locator('option:checked')).toHaveText('Show 1 copy');
  await show.getByRole('button', { name: 'Show menu' }).click();
  await menu.getByRole('menuitem', { name: 'Rename' }).click();
  await show.getByRole('textbox', { name: 'Show name' }).fill('Rehearsal');
  await show.getByRole('textbox', { name: 'Show name' }).press('Enter');
  await expect(picker.locator('option')).toHaveText(['Show 1', 'Rehearsal']);
  await show.getByRole('button', { name: 'Show menu' }).click();
  await menu.getByRole('menuitem', { name: 'Delete' }).click();
  await expect(picker.locator('option')).toHaveText(['Show 1']);
});
