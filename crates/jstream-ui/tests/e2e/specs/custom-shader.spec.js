// The custom shader editor (FL-12, #1232, #1295): Add filter › Custom shader writes an
// effect pack and opens the editor over the Filters window, which shows the shader in
// colour, indents on Tab, compiles a moment after each change and marks the line the
// engine names. The mock engine loads no packs and describes no kinds, so the page's
// socket answers `kind.list`, `kind.describe`, `pack.list`, `pack.read` and `pack.write`
// here, from the packs the test writes; every other message goes on to the mock daemon.
import { test, expect, connect, showPanel } from '../fixtures.js';

const SHARPEN = {
  kind_id: 'sharpen', category: 'video_filter', name: 'Sharpen', version: 1, video: true, audio: false,
  params: [{ id: 'amount', type: 'float', min: 0, max: 1, step: 0.05, default: 0.2, label: 'Amount', live: true }],
};

/** The kind a written custom shader pack has: the template's two settings. */
function shaderKind(pack) {
  return {
    kind_id: `${pack}/shader`, category: 'video_filter', name: 'Custom shader', version: 1, video: true, audio: false,
    params: [
      { id: 'amount', type: 'float', min: 0, max: 1, default: 0.5, label: 'Amount', live: true },
      { id: 'tint', type: 'color', alpha: true, default: '#ff9a3cff', label: 'Tint', live: true },
    ],
  };
}

/** Answers the kind and pack calls from the packs written so far. A shader holding
 * `BROKEN` fails to compile, as the engine reports it, on the line that holds it. */
async function fakePacks(page) {
  const packs = new Map();
  const writes = [];
  const answer = (id, result) => JSON.stringify({ type: 'response', id, result });
  await page.routeWebSocket(/.*/, (ws) => {
    const server = ws.connectToServer();
    ws.onMessage((message) => {
      let parsed = null;
      try {
        parsed = typeof message === 'string' ? JSON.parse(message) : null;
      } catch {
        // Not JSON: pass it on.
      }
      const call = parsed?.call;
      const query = call?.kind === 'query' ? call.query : null;
      const action = call?.kind === 'action' ? call.action : null;
      const kinds = [SHARPEN, ...[...packs.keys()].map(shaderKind)];
      const info = (id) => ({ id, folder: id, kinds: [`${id}/shader`], loaded: true });
      if (query?.type === 'kind.list') {
        const list = kinds.map(({ kind_id, category, name, version }) => ({ kind_id, category, name, version }));
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'kind.list', kinds: list } }));
      } else if (query?.type === 'kind.describe' && kinds.some((k) => k.kind_id === query.kind_id)) {
        const kind = kinds.find((k) => k.kind_id === query.kind_id);
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'kind.describe', kind, settings_schema: {} } }));
      } else if (query?.type === 'pack.list') {
        const list = [...packs.keys()].sort().map(info);
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'pack.list', packs: list } }));
      } else if (query?.type === 'pack.read') {
        const files = (packs.get(query.pack) ?? []).map((f) => ({ path: f.path, size: f.text.length, digest: 'x', text: f.text }));
        ws.send(answer(parsed.id, { kind: 'query', result: { type: 'pack.read', files } }));
      } else if (action?.type === 'pack.write') {
        writes.push(action);
        const shader = action.files.find((f) => f.path === 'shader.wgsl')?.text ?? '';
        const broken = shader.split('\n').findIndex((line) => line.includes('BROKEN'));
        if (broken >= 0) {
          const reason = `shader.wgsl: line ${broken + 1}: unknown identifier \`BROKEN\``;
          ws.send(JSON.stringify({ type: 'error', id: parsed.id, error: { code: 'action_failed', reason, message: reason } }));
          return;
        }
        packs.set(action.pack, action.files);
        ws.send(answer(parsed.id, { kind: 'action', result: { type: 'pack.write', pack: info(action.pack) } }));
      } else {
        server.send(message);
      }
    });
    server.onMessage((message) => ws.send(message));
  });
  return { writes };
}

test('Custom shader writes a pack and opens an editor that colours, indents and compiles it', async ({ page, daemon }) => {
  const { writes } = await fakePacks(page);
  await connect(page, daemon);
  await showPanel(page, 'Filters');
  await page.locator('.sources-panel li.row .row-main').first().click();
  await page.locator('.filters-panel').getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await add.getByRole('listbox', { name: 'Filter type' }).getByRole('option', { name: 'Custom shader' }).click();
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();

  // The template went out as the first custom pack, and the editor opened on it.
  expect(writes.map((w) => w.pack)).toEqual(['custom-1']);
  const editor = page.getByRole('dialog', { name: 'Custom shader' });
  await expect(editor.locator('.modal-sub')).toHaveText('Effect pack “custom-1”');
  const code = editor.getByRole('textbox', { name: 'Shader (WGSL)' });
  await expect(code).toHaveValue(/^\/\/ @name Custom shader\n/);

  // The coloured copy under the text: annotations, comments, keywords, types, calls.
  const shade = editor.locator('.shader-shade');
  await expect(shade).toHaveText((await code.inputValue()) + ' ');
  await expect(shade.locator('.hl-annotation').first()).toHaveText('// @name Custom shader');
  await expect(shade.locator('.hl-annotation').nth(1)).toHaveText('// @param amount float 0..1 = 0.5 "Amount"');
  await expect(shade.locator('.hl-attribute').first()).toHaveText('@fragment');
  await expect(shade.locator('.hl-keyword').first()).toHaveText('fn');
  await expect(shade.locator('.hl-call').first()).toHaveText('fs');
  await expect(shade.locator('.hl-type').first()).toHaveText('vec4');
  // The textarea's own text is transparent, so the colours show through it, and the
  // copy sits exactly under it.
  expect(await code.evaluate((el) => getComputedStyle(el).color)).toBe('rgba(0, 0, 0, 0)');
  const [under, over] = [await shade.boundingBox(), await code.boundingBox()];
  expect(under).toEqual(over);

  // Tab indents rather than leaving the box, and the change compiles.
  await code.focus();
  await code.evaluate((el) => el.setSelectionRange(0, 0));
  await page.keyboard.press('Tab');
  await expect(code).toBeFocused();
  await expect(code).toHaveValue(/^ {4}\/\/ @name Custom shader\n/);
  await expect.poll(() => writes.length).toBe(2);
  await expect(editor.locator('.shader-status')).toHaveText('Compiled. The picture shows this version.');

  // A line the engine refuses is marked in the gutter, and the colours follow the text.
  const text = (await code.inputValue()).replace('let c = src(in.uv);', 'let c = BROKEN;');
  await code.fill(text);
  const line = text.split('\n').findIndex((l) => l.includes('BROKEN')) + 1;
  await expect(editor.locator('.shader-line.bad')).toHaveText(String(line));
  await expect(editor.locator('.shader-status.problem').first()).toContainText(`line ${line}`);
  await expect(shade).toHaveText(text + ' ');

  // Scrolled, the copy scrolls with the text.
  await code.fill(text + '\n'.repeat(80) + '// the end');
  await code.evaluate((el) => { el.scrollTop = 400; });
  await expect.poll(() => shade.evaluate((el) => el.scrollTop)).toBe(await code.evaluate((el) => el.scrollTop));
});
