// A small stand-in for the Design canvas runtime, so the prototype's Design Components
// (`*.dc.html`) run in a plain browser with no build and no network.
//
// It supports what the components use: `{{path}}` bindings in text and attributes,
// `<sc-if value>`, `<sc-for list as>`, `<dc-import name …props>`, `<helmet>` styles, and a
// `class Component extends DCLogic` with React-like `props`, `state`, `setState`,
// `renderVals()`, `componentDidMount()` and `componentWillUnmount()`. Event attributes
// use React names (`onClick`, `onPointerDown`, `onChange`, …) and get native events.
//
// Rendering is a keyless virtual DOM: `sc-if` and `sc-for` keep their slot even when
// empty, so toggling one never shifts the elements after it (as in React).
(function () {
  'use strict';

  // ---- Template parsing --------------------------------------------------------------
  // A tolerant HTML tokenizer that keeps tag and attribute case and, unlike the browser's
  // parser, lets `<sc-for>` sit inside `<select>`.
  const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'source', 'track', 'wbr']);
  const RAW = new Set(['style', 'script', 'textarea', 'title']);
  const decoder = document.createElement('textarea');
  const decode = (s) => {
    if (s.indexOf('&') < 0) return s;
    decoder.innerHTML = s;
    return decoder.value;
  };

  function parse(src) {
    const root = { tag: '#root', attrs: [], children: [] };
    const stack = [root];
    let i = 0;
    const top = () => stack[stack.length - 1];
    const text = (s) => { if (s) top().children.push({ tag: '#text', text: decode(s) }); };
    while (i < src.length) {
      const lt = src.indexOf('<', i);
      if (lt < 0) { text(src.slice(i)); break; }
      text(src.slice(i, lt));
      if (src.startsWith('<!--', lt)) {
        const end = src.indexOf('-->', lt + 4);
        i = end < 0 ? src.length : end + 3;
        continue;
      }
      if (src[lt + 1] === '!' || src[lt + 1] === '?') {
        i = src.indexOf('>', lt) + 1 || src.length;
        continue;
      }
      if (src[lt + 1] === '/') {
        const end = src.indexOf('>', lt);
        // An unclosed tag at the end of a block (a typo in an edited copy) ends the parse.
        if (end < 0) { i = src.length; break; }
        const name = src.slice(lt + 2, end).trim().toLowerCase();
        for (let k = stack.length - 1; k > 0; k--) {
          if (stack[k].tag.toLowerCase() === name) { stack.length = k; break; }
        }
        i = end + 1;
        continue;
      }
      const m = /^<([A-Za-z][\w:-]*)/.exec(src.slice(lt, lt + 64));
      if (!m) { text('<'); i = lt + 1; continue; }
      const node = { tag: m[1], attrs: [], children: [] };
      i = lt + m[0].length;
      const attrRe = /\s*([^\s"'>\/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?|\s*(\/?>)/y;
      for (;;) {
        attrRe.lastIndex = i;
        const a = attrRe.exec(src);
        if (!a) { i = src.indexOf('>', i) + 1 || src.length; break; }
        i = attrRe.lastIndex;
        if (a[5]) { node.selfClose = a[5] === '/>'; break; }
        const v = a[2] ?? a[3] ?? a[4];
        node.attrs.push([a[1], v === undefined ? '' : decode(v)]);
      }
      top().children.push(node);
      const lower = node.tag.toLowerCase();
      if (node.selfClose || VOID.has(lower)) continue;
      if (RAW.has(lower)) {
        const close = new RegExp('</' + lower + '\\s*>', 'ig');
        close.lastIndex = i;
        const c = close.exec(src);
        const end = c ? c.index : src.length;
        node.children.push({ tag: '#text', text: lower === 'style' || lower === 'script' ? src.slice(i, end) : decode(src.slice(i, end)) });
        i = c ? close.lastIndex : src.length;
        continue;
      }
      stack.push(node);
    }
    return root;
  }

  // ---- Bindings ------------------------------------------------------------------------
  const BIND = /\{\{([^}]*)\}\}/g;
  const WHOLE = /^\{\{([^}]*)\}\}$/;

  function lookup(expr, scope) {
    const e = expr.trim();
    if (e === '') return undefined;
    if (e === 'true') return true;
    if (e === 'false') return false;
    if (e === 'null') return null;
    if (e === 'undefined') return undefined;
    if (/^-?\d+(\.\d+)?$/.test(e)) return Number(e);
    if (/^'.*'$|^".*"$/.test(e)) return e.slice(1, -1);
    const parts = e.split('.');
    let v;
    for (let s = scope; s; s = s.up) {
      if (s.vars && Object.prototype.hasOwnProperty.call(s.vars, parts[0])) { v = s.vars[parts[0]]; break; }
    }
    for (let k = 1; k < parts.length && v != null; k++) v = v[parts[k]];
    return v;
  }
  const str = (v) => (v == null || v === false || v === true ? (v === true ? 'true' : '') : String(v));

  // Compiles each attribute once: a whole binding passes its raw value; mixed text is joined.
  function compileValue(raw) {
    const w = WHOLE.exec(raw);
    if (w) return (scope) => lookup(w[1], scope);
    if (raw.indexOf('{{') < 0) return () => raw;
    return (scope) => raw.replace(BIND, (_, e) => str(lookup(e, scope)));
  }

  function prepare(node) {
    if (node.tag === '#text') {
      let t = node.text;
      // JSX-like whitespace: drop indentation-only text, trim around line breaks.
      if (/^\s*$/.test(t) && t.indexOf('\n') >= 0) return null;
      if (t.indexOf('\n') >= 0) t = t.replace(/^\s*\n\s*/, '').replace(/\s*\n\s*$/, '').replace(/\s*\n\s*/g, ' ');
      node.text = t;
      node.val = compileValue(t);
      return node;
    }
    node.props = node.attrs.filter(([k]) => !k.startsWith('hint-')).map(([k, v]) => [k, compileValue(v)]);
    node.children = node.children.map(prepare).filter(Boolean);
    return node;
  }

  // ---- Virtual DOM ---------------------------------------------------------------------
  // A vnode is { type, props, children } for elements, { text } for text, or
  // { comp, props } for an imported component. Children are a list of slots; a slot is
  // a vnode, null (an empty sc-if) or an array (an sc-for or a filled sc-if).
  function build(node, scope, ctx) {
    if (node.tag === '#text') return { text: str(node.val(scope)) };
    const tag = node.tag;
    const lower = tag.toLowerCase();
    const attr = (name) => { const p = node.props.find(([k]) => k === name); return p ? p[1](scope) : undefined; };
    if (lower === 'sc-if') return attr('value') ? buildChildren(node, scope, ctx) : null;
    if (lower === 'sc-for') {
      const list = attr('list');
      const as = (node.attrs.find(([k]) => k === 'as') || [])[1] || 'item';
      if (!list || !list.length) return [];
      return Array.from(list, (item) => buildChildren(node, { vars: { [as]: item }, up: scope }, ctx));
    }
    if (lower === 'helmet') return null;
    if (lower === 'dc-import') {
      const props = {};
      let name;
      for (const [k, f] of node.props) {
        if (k === 'name') name = f(scope);
        else props[k] = f(scope);
      }
      return { comp: name, props };
    }
    const props = {};
    for (const [k, f] of node.props) props[k] = f(scope);
    const children = buildChildren(node, scope, ctx);
    if (lower === 'textarea' && !('value' in props)) {
      props.value = children.map((c) => (c && c.text) || '').join('');
      children.length = 0;
    }
    return { type: tag, props, children };
  }
  const buildChildren = (node, scope, ctx) => node.children.map((c) => build(c, scope, ctx));

  const SVG_NS = 'http://www.w3.org/2000/svg';
  const EVENT_ALIAS = { doubleclick: 'dblclick', focus: 'focusin', blur: 'focusout' };

  function eventName(key, el) {
    let ev = key.slice(2).toLowerCase();
    ev = EVENT_ALIAS[ev] || ev;
    // React's onChange fires on every edit for text fields and sliders.
    if (ev === 'change') {
      const t = el.tagName.toLowerCase();
      const type = (el.getAttribute('type') || '').toLowerCase();
      if (t === 'textarea' || (t === 'input' && type !== 'checkbox' && type !== 'radio' && type !== 'file')) ev = 'input';
    }
    return ev;
  }
  function proxy(e) { const h = this._h && this._h[e.type]; if (h) h(e); }

  function setProp(el, key, v, old) {
    if (key.length > 2 && key[0] === 'o' && key[1] === 'n') {
      const ev = eventName(key, el);
      el._h = el._h || {};
      if (typeof v === 'function') {
        if (!el._h[ev]) el.addEventListener(ev, proxy);
        el._h[ev] = v;
      } else if (el._h[ev]) { el.removeEventListener(ev, proxy); delete el._h[ev]; }
      return;
    }
    if (key === 'style') {
      if (typeof v === 'object' && v) { el.removeAttribute('style'); Object.assign(el.style, v); }
      else if (v == null || v === false) el.removeAttribute('style');
      else if (el.getAttribute('style') !== v) el.setAttribute('style', v);
      return;
    }
    if (key === 'className') key = 'class';
    if (key === 'value' && 'value' in el && el.tagName !== 'BUTTON' && el.tagName !== 'OPTION' && el.tagName !== 'LI') {
      const s = v == null ? '' : String(v);
      if (el.value !== s) el.value = s;
      return;
    }
    if (key === 'checked' || key === 'selected') { el[key] = !!v; return; }
    if (v == null || (v === false && !/^(aria|data)-/.test(key) && key !== 'draggable' && key !== 'spellcheck')) {
      el.removeAttribute(key);
      return;
    }
    const s = v === true && !/^(aria|data)-/.test(key) && key !== 'draggable' && key !== 'spellcheck' ? '' : String(v);
    if (el.getAttribute(key) !== s) el.setAttribute(key, s);
  }

  function setProps(el, props, old) {
    // `type` first, so a value lands on the right kind of input.
    if (props.type !== undefined && props.type !== old.type) setProp(el, 'type', props.type, old.type);
    for (const k in old) if (!(k in props)) setProp(el, k, null, old[k]);
    for (const k in props) {
      if (k === 'type') continue;
      if (k === 'value' || k === 'checked' || props[k] !== old[k]) setProp(el, k, props[k], old[k]);
    }
  }

  // Renders `nu` against `old` (either may be null) and returns the new vnode with its DOM
  // attached (`dom` for nodes, `inst` for components). Never touches the parent's child list.
  function patch(old, nu, svg, depth) {
    if (nu == null) { if (old) unmount(old); return null; }
    if (Array.isArray(nu)) {
      const o = Array.isArray(old) ? old : (old && unmount(old), []);
      const out = nu.map((c, i) => patch(o[i], c, svg, depth));
      for (let i = nu.length; i < o.length; i++) unmount(o[i]);
      return out;
    }
    if (Array.isArray(old)) { unmount(old); old = null; }
    if ('text' in nu) {
      if (old && 'text' in old) {
        if (old.text !== nu.text) old.dom.nodeValue = nu.text;
        nu.dom = old.dom;
      } else { if (old) unmount(old); nu.dom = document.createTextNode(nu.text); }
      return nu;
    }
    if (nu.comp) {
      if (old && old.comp === nu.comp && old.inst) {
        nu.inst = old.inst;
        nu.inst.props = withDefaults(nu.inst.constructor, nu.props);
        renderInst(nu.inst);
      } else {
        if (old) unmount(old);
        nu.inst = createInst(nu.comp, nu.props, depth + 1);
      }
      return nu;
    }
    const isSvg = svg || nu.type.toLowerCase() === 'svg';
    if (old && old.type === nu.type && old.dom) {
      nu.dom = old.dom;
      nu.children = patch(old.children, nu.children, isSvg && nu.type !== 'foreignObject', depth);
      placeChildren(nu.dom, nu.children);
      setProps(nu.dom, nu.props, old.props);
    } else {
      if (old) unmount(old);
      nu.dom = isSvg ? document.createElementNS(SVG_NS, nu.type) : document.createElement(nu.type);
      nu.children = patch(null, nu.children, isSvg && nu.type !== 'foreignObject', depth);
      placeChildren(nu.dom, nu.children);
      setProps(nu.dom, nu.props, {});
    }
    return nu;
  }

  function domNodes(v, out = []) {
    if (v == null) return out;
    if (Array.isArray(v)) { for (const c of v) domNodes(c, out); return out; }
    if (v.inst) return domNodes(v.inst.tree, out);
    if (v.dom) out.push(v.dom);
    return out;
  }

  // Makes `el`'s children exactly the DOM of `slots`, moving as little as possible (moving a
  // focused input would blur it).
  function placeChildren(el, slots) {
    const want = domNodes(slots);
    const keep = new Set(want);
    for (let n = el.firstChild; n;) { const next = n.nextSibling; if (!keep.has(n)) el.removeChild(n); n = next; }
    let cur = el.firstChild;
    for (const n of want) {
      if (n === cur) { cur = cur.nextSibling; continue; }
      el.insertBefore(n, cur);
    }
  }

  function unmount(v) {
    if (v == null) return;
    if (Array.isArray(v)) { v.forEach(unmount); return; }
    if (v.inst) {
      const inst = v.inst;
      inst._dead = true;
      if (inst.componentWillUnmount) try { inst.componentWillUnmount(); } catch (e) { console.error(e); }
      unmount(inst.tree);
      return;
    }
    if (v.children) unmount(v.children);
  }

  // ---- Components ----------------------------------------------------------------------
  const registry = {};
  let mountQueue = [];

  function withDefaults(C, props) {
    const out = { ...props };
    const d = C.__defaults || {};
    for (const k in d) if (out[k] === undefined) out[k] = d[k];
    return out;
  }

  class DCLogic {
    constructor(props) { this.props = props; this.state = {}; }
    setState(update, cb) {
      (this._queue = this._queue || []).push(update);
      if (cb) (this._cbs = this._cbs || []).push(cb);
      schedule(this);
    }
    forceUpdate() { schedule(this); }
    renderVals() { return {}; }
  }

  function createInst(name, props, depth) {
    const def = registry[name];
    if (!def) throw new Error('Unknown component: ' + name);
    const C = def.Component;
    const inst = new C(withDefaults(C, props));
    inst._def = def;
    inst._depth = depth;
    inst.tree = null;
    renderInst(inst);
    if (inst.componentDidMount) mountQueue.push(inst);
    return inst;
  }

  function applyQueue(inst) {
    if (!inst._queue) return;
    let s = inst.state;
    for (const u of inst._queue) {
      const part = typeof u === 'function' ? u(s, inst.props) : u;
      if (part) s = { ...s, ...part };
    }
    inst._queue = null;
    inst.state = s;
  }

  function renderInst(inst) {
    applyQueue(inst);
    inst._dirty = false;
    const vals = inst.renderVals() || {};
    const scope = { vars: vals };
    const nu = inst._def.root.children.map((c) => build(c, scope));
    const before = inst.tree ? domNodes(inst.tree) : null;
    inst.tree = patch(inst.tree, nu, false, inst._depth);
    // A self-render can swap the root nodes; put the new ones where the old ones were.
    if (before && before.length && before[0].parentNode) {
      const after = domNodes(inst.tree);
      const parent = before[0].parentNode;
      if (after.length !== before.length || after.some((n, i) => n !== before[i])) {
        const anchor = before[before.length - 1].nextSibling;
        for (const n of before) if (after.indexOf(n) < 0 && n.parentNode === parent) parent.removeChild(n);
        for (const n of after) parent.insertBefore(n, anchor && anchor.parentNode === parent ? anchor : null);
      }
    }
    const cbs = inst._cbs;
    inst._cbs = null;
    if (cbs) mountQueue.push(...cbs.map((f) => ({ cb: f, inst })));
  }

  let dirty = [];
  let scheduled = false;
  function schedule(inst) {
    if (inst._dirty) return;
    inst._dirty = true;
    dirty.push(inst);
    if (!scheduled) { scheduled = true; Promise.resolve().then(flush); }
  }
  function flush() {
    scheduled = false;
    while (dirty.length) {
      const list = dirty.sort((a, b) => a._depth - b._depth);
      dirty = [];
      for (const inst of list) if (inst._dirty && !inst._dead) {
        try { renderInst(inst); } catch (e) { console.error(e); }
      }
      runMounts();
    }
  }
  function runMounts() {
    const q = mountQueue;
    mountQueue = [];
    for (const m of q) {
      try {
        if (m.cb) m.cb.call(m.inst);
        else m.componentDidMount();
      } catch (e) { console.error(e); }
    }
  }

  // ---- Loading -------------------------------------------------------------------------
  const helmetDone = new Set();

  function define(name, markup, logic, propsJson) {
    const root = prepare(parse(markup));
    const xdc = root.children.find((c) => c.tag && c.tag.toLowerCase() === 'x-dc') || root;
    // <helmet> styles go into the document head once per component.
    for (const h of xdc.children.filter((c) => c.tag && c.tag.toLowerCase() === 'helmet')) {
      if (helmetDone.has(name)) break;
      helmetDone.add(name);
      for (const s of h.children.filter((c) => c.tag && c.tag.toLowerCase() === 'style')) {
        const el = document.createElement('style');
        el.setAttribute('data-dc', name);
        el.textContent = (s.children[0] && s.children[0].text) || '';
        document.head.appendChild(el);
      }
    }
    const Component = new Function('DCLogic', logic + '\n;return Component;')(DCLogic);
    const defaults = {};
    let spec = {};
    try { spec = JSON.parse(propsJson || '{}'); } catch (e) { console.error('Bad data-props for ' + name, e); }
    for (const k in spec) if (k[0] !== '$' && spec[k] && 'default' in spec[k]) defaults[k] = spec[k].default;
    Component.__defaults = defaults;
    registry[name] = { root: xdc, Component, preview: spec.$preview || null };
  }

  // Mounts component `name` into `host`, replacing what was there.
  let current = null;
  function mount(name, host) {
    if (current) unmount(current.v);
    host.textContent = '';
    const v = { comp: name, props: {} };
    v.inst = createInst(name, {}, 0);
    placeChildren(host, [v]);
    current = { v, host };
    runMounts();
  }

  function loadAll() {
    for (const el of document.querySelectorAll('script[type="text/x-dc-markup"]')) {
      const name = el.getAttribute('data-dc');
      const logicEl = document.querySelector('script[type="text/x-dc-logic"][data-dc="' + name + '"]');
      try {
        define(name, el.textContent, logicEl ? logicEl.textContent : 'class Component extends DCLogic {}', logicEl && logicEl.getAttribute('data-props'));
      } catch (e) { console.error('Could not load ' + name, e); }
    }
  }

  window.DC = { define, mount, loadAll, registry };
})();
