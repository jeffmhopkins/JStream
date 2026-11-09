// The custom shader editor (FL-12, #1232, #1295): Add filter › Custom shader writes an
// effect pack and opens the editor over the Filters window, which shows the shader in
// colour, indents on Tab, compiles a moment after each change and marks the line the
// engine names. Save as pack… writes it under a chosen ID and moves the filter there,
// and an `@renamed` line raises the manifest's version with its `renames`. Add source ›
// Shader (ADR-0135) writes a source pack, places its source and opens the same editor
// over its Properties, which open it again from Edit shader…. The mock engine loads no packs and describes no kinds, so the page's
// socket answers `kind.list`, `kind.describe`, `pack.list`, `pack.read` and `pack.write`
// here, from the packs the test writes, and refuses a filter of a written pack with a
// setting its kind doesn't declare; every other message goes on to the mock daemon.
import { test, expect, connect, openAddSource, showPanel } from '../fixtures.js';

const SHARPEN = {
  kind_id: 'sharpen', category: 'video_filter', name: 'Sharpen', version: 1, video: true, audio: false,
  params: [{ id: 'amount', type: 'float', min: 0, max: 1, step: 0.05, default: 0.2, label: 'Amount', live: true }],
};

/** The kind a written custom shader pack has: a source when its manifest has a
 * `[[source]]` (ADR-0135), else a video filter, with its manifest's name and version, and
 * a setting per `// @param` line of its shader (a colour or a float). */
function shaderKind(pack, files) {
  const manifest = files.find((f) => f.path === 'effect.toml')?.text ?? '';
  const shader = files.find((f) => f.path === 'shader.wgsl')?.text ?? '';
  const name = /^name = "(.*)"$/m.exec(manifest)?.[1] ?? 'Custom shader';
  const version = Number(/^version = (\d+)$/m.exec(manifest)?.[1] ?? 1);
  const params = [...shader.matchAll(/^\/\/ @param (\S+) (\S+)/gm)].map(([, id, type]) => (type === 'color'
    ? { id, type: 'color', alpha: true, default: '#ff9a3cff', label: id, live: true }
    : { id, type: 'float', min: 0, max: 1, default: 0.5, label: id, live: true }));
  const category = /^\[\[source\]\]$/m.test(manifest) ? 'source' : 'video_filter';
  return { kind_id: `${pack}/shader`, category, name, version, video: true, audio: false, params };
}

/** Adds a Custom shader to the Backdrop from Add filter, and returns its editor. */
async function addCustomShader(page) {
  await showPanel(page, 'Filters');
  await page.locator('.sources-panel li.row .row-main').first().click();
  await page.locator('.filters-panel').getByRole('button', { name: '+ Add filter…' }).click();
  const add = page.getByRole('dialog', { name: 'Add filter' });
  await add.getByRole('listbox', { name: 'Filter type' }).getByRole('option', { name: 'Custom shader' }).click();
  await add.getByRole('button', { name: 'Add filter' }).click();
  await expect(add).toBeHidden();
  return page.getByRole('dialog', { name: 'Custom shader' });
}

/** Answers the kind and pack calls from the packs written so far. A shader holding
 * `BROKEN` fails to compile, as the engine reports it, on the line that holds it. A
 * batch adding a filter of a written pack with a setting its kind doesn't declare is
 * refused, as the engine refuses it; the others go on to the mock daemon. */
async function fakePacks(page) {
  const packs = new Map();
  const writes = [];
  const adds = [];
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
      const kinds = [SHARPEN, ...[...packs].map(([pack, files]) => shaderKind(pack, files))];
      const info = (id) => ({ id, folder: id, kinds: [`${id}/shader`], loaded: true });
      const added = call?.kind === 'batch' ? call.edits.filter((e) => e.type === 'filter.add') : [];
      const stale = added.flatMap((edit) => {
        const kind = kinds.find((k) => k.kind_id === edit.kind && packs.has(edit.kind.split('/')[0]));
        return kind ? Object.keys(edit.settings ?? {}).filter((id) => !kind.params.some((p) => p.id === id)) : [];
      });
      adds.push(...added);
      if (stale.length > 0) {
        const reason = `\`${stale[0]}\` is not a parameter of kind`;
        const error = { code: 'invalid_argument', field: 'settings', reason, message: `invalid \`settings\`: ${reason}` };
        ws.send(JSON.stringify({ type: 'error', id: parsed.id, error }));
      } else if (query?.type === 'kind.list') {
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
  return { writes, adds };
}

test('Custom shader writes a pack and opens an editor that colours, indents and compiles it', async ({ page, daemon }) => {
  const { writes } = await fakePacks(page);
  await connect(page, daemon);
  const editor = await addCustomShader(page);

  // The template went out as the first custom pack, and the editor opened on it.
  expect(writes.map((w) => w.pack)).toEqual(['custom-1']);
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

  // Scrolled, the copy scrolls with the text, to the very end of both ways, where the
  // text's scrollbars would leave a copy with none behind.
  await code.fill(text + '\n'.repeat(80) + `// the end ${'x'.repeat(300)}`);
  await code.evaluate((el) => { el.scrollTop = 400; });
  await expect.poll(() => shade.evaluate((el) => el.scrollTop)).toBe(await code.evaluate((el) => el.scrollTop));
  await code.evaluate((el) => { el.scrollTop = el.scrollHeight; el.scrollLeft = el.scrollWidth; });
  const end = await code.evaluate((el) => [el.scrollTop, el.scrollLeft]);
  await expect.poll(() => shade.evaluate((el) => [el.scrollTop, el.scrollLeft])).toEqual(end);
});

test('Save as pack… writes the shader under a new ID and moves the filter to it', async ({ page, daemon }) => {
  const { writes, adds } = await fakePacks(page);
  await connect(page, daemon);
  let editor = await addCustomShader(page);
  await expect(editor.locator('.modal-sub')).toHaveText('Effect pack “custom-1”');
  // A value for `amount`, set in the Filters window, for the move to carry.
  await editor.getByRole('button', { name: 'Done' }).click();
  await expect(editor).toBeHidden();
  const window = page.getByRole('dialog', { name: 'Filters', exact: true });
  const amount = window.getByRole('spinbutton').first();
  await amount.fill('0.25');
  await amount.press('Tab');
  await window.getByRole('button', { name: 'Edit shader…' }).click();
  await expect(editor.locator('.modal-sub')).toHaveText('Effect pack “custom-1”');
  const code = editor.getByRole('textbox', { name: 'Shader (WGSL)' });

  // A rename: the manifest goes to version 2 with the old ID in `renames`.
  const renamed = (await code.inputValue())
    .replace('// @param amount float', '// @param mix_amount float')
    .replace('p.amount', 'p.mix_amount')
    .replace('// @name Custom shader', '// @name Warm look\n// @renamed amount mix_amount');
  await code.fill(renamed);
  await expect(editor.locator('.shader-status')).toHaveText('Compiled. The picture shows this version.');
  const manifest = writes.at(-1).files.find((f) => f.path === 'effect.toml').text;
  expect(manifest).toContain('version = 2\n');
  expect(manifest).toContain('renames = { amount = "mix_amount" }');
  await expect(editor.locator('.shader-shade .hl-annotation').nth(1)).toHaveText('// @renamed amount mix_amount');

  // Save as pack…: an ID already taken is refused, a new one is written and the filter
  // moves to its kind, with the editor open on the new pack.
  await editor.getByRole('button', { name: 'Save as pack…' }).click();
  const id = editor.getByRole('textbox', { name: 'Pack ID' });
  await id.fill('custom-1');
  await editor.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(editor.getByRole('alert')).toHaveText('A pack called `custom-1` already exists.');
  await id.fill('Warm');
  await editor.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(editor.getByRole('alert')).toContainText('A pack ID is 1 to 32');
  const before = writes.length;
  await id.fill('warm-look');
  await editor.getByRole('button', { name: 'Save', exact: true }).click();
  await expect.poll(() => writes.length).toBe(before + 1);
  expect(writes.at(-1).pack).toBe('warm-look');
  expect(writes.at(-1).files.find((f) => f.path === 'shader.wgsl').text).toBe(renamed);
  expect(writes.at(-1).files.find((f) => f.path === 'effect.toml').text).toContain('id = "warm-look"');

  editor = page.getByRole('dialog', { name: 'Custom shader' });
  await expect(editor.locator('.modal-sub')).toHaveText('Effect pack “warm-look”');
  await expect(editor.getByRole('textbox', { name: 'Shader (WGSL)' })).toHaveValue(renamed);
  await editor.getByRole('button', { name: 'Done' }).click();
  await expect(editor).toBeHidden();
  // The Backdrop's chain holds one filter of the new kind in the old one's place.
  const rows = page.locator('.filters-panel').getByRole('list', { name: 'Video filters' }).getByRole('listitem');
  await expect(rows.locator('.filter-name')).toHaveText(['Sharpen', 'Blur', 'Warm look']);
  // The new filter took the value under its new ID, and nothing the kind doesn't declare.
  expect(adds.at(-1).kind).toBe('warm-look/shader');
  expect(adds.at(-1).settings).toEqual({ mix_amount: 0.25 });
});

test('Add source › Shader writes a source pack, places it and opens the editor over its Properties', async ({ page, daemon }) => {
  const { writes } = await fakePacks(page);
  await connect(page, daemon);
  const add = await openAddSource(page);
  await add.getByRole('option', { name: 'Shader' }).click();
  await add.getByRole('button', { name: 'Create', exact: true }).click();
  await expect(add).toBeHidden();

  // The source template went out as the first custom pack, a `[[source]]` with one pass.
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].pack).toBe('custom-1');
  let manifest = writes[0].files.find((f) => f.path === 'effect.toml').text;
  expect(manifest).toContain('[[source]]\nid = "shader"\nname = "Shader source"\n');
  expect(manifest).toContain('width = 1920\nheight = 1080\n');
  expect(manifest).toContain('[[source.pass]]\nid = "previous"\nshader = "shader.wgsl"\ninputs = []\n');

  // The source is placed in the scene, and its Properties open with the editor over them.
  await expect(page.locator('.sources-panel li.row')).toHaveCount(2);
  const props = page.getByRole('dialog', { name: 'Properties' });
  await expect(props).toBeVisible();
  const editor = page.getByRole('dialog', { name: 'Custom shader' });
  await expect(editor.locator('.modal-sub')).toHaveText('Effect pack “custom-1”');
  const code = editor.getByRole('textbox', { name: 'Shader (WGSL)' });
  await expect(code).toHaveValue(/^\/\/ @name Shader source\n\/\/ @size 1920x1080\n/);
  await expect(editor.locator('.shader-shade .hl-annotation').first()).toHaveText('// @name Shader source');
  // A source can't move to another kind, so it has no Save as pack….
  await expect(editor.getByRole('button', { name: 'Save as pack…' })).toHaveCount(0);

  // The source annotations reach the manifest: a size, and feedback as the pass's input.
  const edited = (await code.inputValue())
    .replace('// @name Shader source', '// @name Plasma')
    .replace('// @size 1920x1080', '// @size 640x360\n// @feedback');
  await code.fill(edited);
  await expect(editor.locator('.shader-status')).toHaveText('Compiled. The picture shows this version.');
  manifest = writes.at(-1).files.find((f) => f.path === 'effect.toml').text;
  expect(writes.at(-1).pack).toBe('custom-1');
  expect(manifest).toContain('name = "Plasma"\n');
  expect(manifest).toContain('width = 640\nheight = 360\n');
  expect(manifest).toContain('inputs = ["previous"]');

  // A setting that takes a source setting's ID is refused before anything is written.
  const before = writes.length;
  await code.fill(edited.replace('// @feedback', '// @feedback\n// @param width float 0..1 = 0.5 "Width"'));
  await expect(editor.locator('.shader-status.problem').first()).toContainText('width');
  expect(writes.length).toBe(before);
  await code.fill(edited);
  await expect(editor.locator('.shader-status')).toHaveText('Compiled. The picture shows this version.');

  // Done closes the editor over Properties, whose Edit shader… opens it again on the pack.
  await editor.getByRole('button', { name: 'Done' }).click();
  await expect(editor).toBeHidden();
  await expect(props).toBeVisible();
  await props.getByRole('button', { name: 'Edit shader…' }).click();
  await expect(editor.locator('.modal-sub')).toHaveText('Effect pack “custom-1”');
  await expect(code).toHaveValue(edited);
  await editor.getByRole('button', { name: 'Done' }).click();
  await expect(editor).toBeHidden();

  // Add source now lists the written shader by its name, beside Shader.
  await props.getByRole('button', { name: 'Close' }).first().click();
  const again = await openAddSource(page);
  await expect(again.getByRole('option', { name: 'Plasma' })).toBeVisible();
  await expect(again.getByRole('option', { name: 'Shader', exact: true })).toBeVisible();
});
