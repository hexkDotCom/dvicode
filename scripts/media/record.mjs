// Records the README animation and screenshots (docs/images) on the demo project, compiled
// with the real UVM library: starts VS Code with an empty profile and the published DVICode,
// drives it through its DevTools port, and records the window.
//
//   node scripts/media/record.mjs <uvm-home> [out-dir] [gif|gif-mouse|gif-edit|variants|shots|all]
//
// Windows, with VS Code installed and ffmpeg on the PATH. Leave the window alone while it runs.
// Writes <out-dir>/dvicode-demo.gif and .mp4 (keyboard), dvicode-demo-mouse.* (mouse only),
// dvicode-demo-edit.* (mouse, a wrong type fixed from the lightbulb and a missing `;`: the README
// animation), hover.png, hierarchy.png and problems.png.
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const [uvmHome, outArg = path.join(repo, 'build', 'media'), what = 'all'] = process.argv.slice(2);
if (!uvmHome || !fs.existsSync(path.join(uvmHome, 'src', 'uvm_pkg.sv'))) {
  console.error('usage: node scripts/media/record.mjs <uvm-home (with src/uvm_pkg.sv)> [out-dir] [gif|gif-mouse|gif-edit|variants|shots|all]');
  process.exit(2);
}
const out = path.resolve(outArg);
const ws = path.join(out, 'ws', 'demo-project');
const CODE = path.join(process.env.LOCALAPPDATA, 'Programs', 'Microsoft VS Code', 'Code.exe');
const CLI = path.join(process.env.LOCALAPPDATA, 'Programs', 'Microsoft VS Code', 'bin', 'code.cmd');
const PORT = 9339;
const W = 1280, H = 760;
const profile = path.join(out, 'profile');
const exts = path.join(out, 'extensions');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** The demo project, with the real UVM library (`-uvm`) instead of its stand-in. */
function prepareWorkspace() {
  fs.rmSync(path.dirname(ws), { recursive: true, force: true, maxRetries: 10, retryDelay: 500 });
  fs.cpSync(path.join(repo, 'examples', 'demo-project'), ws, { recursive: true });
  fs.rmSync(path.join(ws, 'tb', 'uvm'), { recursive: true, force: true });
  for (const b of ['default.build', 'errors.build']) {
    const f = path.join(ws, '.dvicode', b);
    const text = fs.readFileSync(f, 'utf8').split(/\r?\n/).filter((l) => !/^\+incdir\+.*tb\/uvm$/i.test(l)).map((l) => (/tb\/uvm\/uvm_pkg\.sv$/i.test(l) ? '-uvm' : l));
    fs.writeFileSync(f, text.join('\n'));
  }
}

function prepareProfile() {
  fs.rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 500 });
  fs.mkdirSync(path.join(profile, 'User'), { recursive: true });
  fs.writeFileSync(path.join(profile, 'User', 'settings.json'), JSON.stringify({
    'workbench.startupEditor': 'none',
    'workbench.tips.enabled': false,
    'workbench.colorTheme': 'Default Dark Modern',
    'workbench.secondarySideBar.defaultVisibility': 'hidden',
    'workbench.layoutControl.enabled': false,
    'workbench.editor.empty.hint': 'hidden',
    'chat.disableAIFeatures': true,
    'chat.commandCenter.enabled': false,
    'update.mode': 'none',
    'update.showReleaseNotes': false,
    'telemetry.telemetryLevel': 'off',
    'security.workspace.trust.enabled': false,
    'extensions.ignoreRecommendations': true,
    'extensions.autoUpdate': false,
    'git.enabled': false,
    'window.restoreWindows': 'none',
    'editor.fontSize': 15,
    'editor.lineHeight': 22,
    'editor.minimap.enabled': false,
    'editor.stickyScroll.enabled': false,
    'editor.hover.delay': 700,
    'editor.inlayHints.enabled': 'off',
    'editor.renderWhitespace': 'none',
    'breadcrumbs.enabled': false,
    'window.menuStyle': 'custom',
    'editor.lightbulb.enabled': 'on',
    'dvicode.uvmHome': uvmHome,
  }, null, 2));
  // Closes the unsaved macro expansion without a visible command.
  fs.writeFileSync(path.join(profile, 'User', 'keybindings.json'), JSON.stringify([{ key: 'ctrl+alt+shift+f9', command: 'workbench.action.revertAndCloseActiveEditor' }]));
  if (!fs.existsSync(exts) || !fs.readdirSync(exts).some((d) => d.startsWith('hexk.dvicode'))) {
    execFileSync(`"${CLI}"`, ['--extensions-dir', `"${exts}"`, '--user-data-dir', `"${profile}"`, '--install-extension', 'hexk.dvicode', '--force'], { stdio: 'inherit', shell: true });
  }
}

class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.id = 0;
    this.pending = new Map();
    this.handlers = new Map();
    this.ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.id && this.pending.has(m.id)) {
        const { res, rej } = this.pending.get(m.id);
        this.pending.delete(m.id);
        m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result);
      } else if (m.method && this.handlers.has(m.method)) {
        this.handlers.get(m.method)(m.params);
      }
    };
  }
  open() { return new Promise((r, j) => { this.ws.onopen = r; this.ws.onerror = j; }); }
  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((res, rej) => this.pending.set(id, { res, rej }));
  }
  on(method, f) { this.handlers.set(method, f); }
  async eval(expr) {
    const r = await this.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.text + ' ' + JSON.stringify(r.exceptionDetails.exception?.description));
    return r.result.value;
  }
}

let cdp;
let mouse = { x: W / 2, y: H / 2 };

const KEYS = {
  Enter: [13, 'Enter'], Escape: [27, 'Escape'], F12: [123, 'F12'], ArrowLeft: [37, 'ArrowLeft'], ArrowRight: [39, 'ArrowRight'],
  ArrowDown: [40, 'ArrowDown'], Tab: [9, 'Tab'], Backspace: [8, 'Backspace'], '.': [190, '.', 'Period'], F9: [120, 'F9'], End: [35, 'End'],
};
async function key(spec) {
  // "ctrl+shift+p", "F12", "alt+ArrowLeft"
  const parts = spec.split('+');
  const k = parts.pop();
  const mods = (parts.includes('alt') ? 1 : 0) | (parts.includes('ctrl') ? 2 : 0) | (parts.includes('shift') ? 8 : 0);
  let vk, keyName, code;
  if (KEYS[k]) {
    [vk, keyName, code = keyName] = KEYS[k];
  } else {
    vk = k.toUpperCase().charCodeAt(0);
    keyName = k;
    code = /[a-z]/i.test(k) ? 'Key' + k.toUpperCase() : 'Digit' + k;
  }
  await cdp.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', modifiers: mods, windowsVirtualKeyCode: vk, key: keyName, code });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', modifiers: mods, windowsVirtualKeyCode: vk, key: keyName, code });
  await sleep(120);
}
async function type(text, delay = 55) {
  for (const ch of text) {
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', text: ch, key: ch, unmodifiedText: ch });
    await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: ch });
    await sleep(delay);
  }
}
async function moveTo(x, y, steps = 18) {
  const { x: x0, y: y0 } = mouse;
  for (let i = 1; i <= steps; i++) {
    const t = i / steps, e = t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
    const px = x0 + (x - x0) * e, py = y0 + (y - y0) * e;
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: px, y: py });
    await cdp.eval(`window.__ptr && window.__ptr(${px}, ${py})`);
    await sleep(16);
  }
  mouse = { x, y };
}
async function click(x, y) {
  await moveTo(x, y);
  await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 });
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: 1 });
  await sleep(250);
}
async function waitFor(expr, ms = 20000, what = expr) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    try {
      const v = await cdp.eval(expr);
      if (v) return v;
    } catch {}
    await sleep(150);
  }
  throw new Error('timed out waiting for ' + what);
}

// A pointer and a caption, drawn over the workbench.
const OVERLAY = `(() => {
  if (window.__ptr) return;
  const p = document.createElement('div');
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('width', '22'); svg.setAttribute('height', '30'); svg.setAttribute('viewBox', '0 0 22 30');
  const path = document.createElementNS(NS, 'path');
  path.setAttribute('d', 'M2 2 L2 24 L8 18 L12 28 L16 26 L12 17 L20 17 Z');
  path.setAttribute('fill', 'white'); path.setAttribute('stroke', 'black'); path.setAttribute('stroke-width', '1.6'); path.setAttribute('stroke-linejoin', 'round');
  svg.appendChild(path);
  p.appendChild(svg);
  Object.assign(p.style, { position: 'fixed', left: '0px', top: '0px', zIndex: 100000, pointerEvents: 'none', filter: 'drop-shadow(0 1px 2px rgba(0,0,0,.6))' });
  document.body.appendChild(p);
  window.__ptr = (x, y) => { p.style.left = (x - 2) + 'px'; p.style.top = (y - 2) + 'px'; };
  window.__ptrShow = (v) => { p.style.display = v ? '' : 'none'; };
  const c = document.createElement('div');
  Object.assign(c.style, { position: 'fixed', left: '50%', bottom: '44px', transform: 'translateX(-50%)', zIndex: 100001, pointerEvents: 'none',
    background: 'rgba(24,32,48,.94)', color: '#F2F6FC', border: '1.5px solid #40C4FF', borderRadius: '8px', padding: '9px 18px',
    font: '600 17px "Segoe UI", sans-serif', boxShadow: '0 6px 20px rgba(0,0,0,.45)', transition: 'opacity .25s', opacity: '0', whiteSpace: 'nowrap' });
  document.body.appendChild(c);
  window.__cap = (t) => { if (t) { c.textContent = t; c.style.opacity = '1'; } else { c.style.opacity = '0'; } };
})()`;
const caption = (t) => cdp.eval(`window.__cap(${JSON.stringify(t)})`);

async function command(title) {
  await key('ctrl+shift+p');
  await waitFor(`!!document.querySelector('.quick-input-widget:not([style*="display: none"]) input')`);
  await type(title, 35);
  await sleep(500);
  await key('Enter');
  await sleep(400);
}
/** Picks `text` in the quick pick a command opened, once it is open. */
async function pick(text) {
  await waitFor(`(() => { const i = document.querySelector('.quick-input-widget:not([style*="display: none"]) input'); return i && document.activeElement === i && i.value === ''; })()`, 8000, 'quick pick');
  await type(text, 40);
  await sleep(500);
  await key('Enter');
}
async function openFile(name) {
  await key('ctrl+p');
  await waitFor(`!!document.querySelector('.quick-input-widget:not([style*="display: none"]) input')`);
  await type(name, 45);
  await sleep(600);
  await key('Enter');
  await waitFor(`[...document.querySelectorAll('.tab.active .label-name')].some(e => e.textContent.includes(${JSON.stringify(name)}))`, 8000, 'tab ' + name);
  await sleep(400);
}
async function gotoLine(line, col) {
  await key('ctrl+g');
  await sleep(250);
  await type(`${line}:${col}`, 30);
  await key('Enter');
  await sleep(350);
}
/** Screen position of the text cursor in the active editor. */
async function cursorPos() {
  return cdp.eval(`(() => { const ed = [...document.querySelectorAll('.editor-group-container.active .monaco-editor .cursor')].find(c => c.offsetParent); const r = ed.getBoundingClientRect(); return { x: r.left, y: r.top + r.height / 2 }; })()`);
}
/** Screen position of `text` on the visible line that contains it (first match). */
async function textPos(text, nth = 0) {
  return cdp.eval(`(() => {
    const ed = document.querySelector('.editor-group-container.active .monaco-editor .view-lines');
    const lines = [...ed.querySelectorAll('.view-line')].sort((a, b) => a.getBoundingClientRect().top - b.getBoundingClientRect().top);
    let n = ${nth};
    for (const l of lines) {
      const walker = document.createTreeWalker(l, NodeFilter.SHOW_TEXT);
      let full = '', nodes = [];
      while (walker.nextNode()) { nodes.push([walker.currentNode, full.length]); full += walker.currentNode.textContent.replace(/\\u00a0/g, ' '); }
      const i = full.indexOf(${JSON.stringify(text)});
      if (i < 0) continue;
      if (n-- > 0) continue;
      const mid = i + Math.floor(${JSON.stringify(text)}.length / 2);
      const [node, off] = nodes.filter(([, o]) => o <= mid).pop();
      const range = document.createRange();
      range.setStart(node, mid - off); range.setEnd(node, mid - off + 1);
      const r = range.getBoundingClientRect();
      return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
    }
    return null;
  })()`);
}
async function waitEngineReady() {
  await waitFor(`(() => { const it = [...document.querySelectorAll('#workbench\\\\.parts\\\\.statusbar .statusbar-item')].find(e => /DVICode/.test(e.textContent) || /dvicode/i.test(e.id)); return it && !it.querySelector('.codicon-loading, .codicon-sync, [class*="spin"]'); })()`, 60000, 'DVICode ready');
}
async function closeAllEditors() {
  await command('View: Close All Editors');
  await sleep(300);
}
async function hideHover() {
  await moveTo(W - 60, H - 120, 8);
  await key('Escape');
  await sleep(300);
}
async function rowsOf(view) {
  return cdp.eval(`[...document.querySelectorAll('[id="workbench.view.extension.dvicode"] .pane')].filter(p => p.querySelector('.pane-header')?.textContent.includes(${JSON.stringify(view)})).flatMap(p => [...p.querySelectorAll('.monaco-list-row')]).map(r => { const b = r.getBoundingClientRect(); return { label: r.textContent.trim(), expanded: r.getAttribute('aria-expanded'), x: b.left + 40, y: b.top + b.height / 2 }; })`);
}
async function collapsePane(view) {
  const p = await cdp.eval(`(() => { const h = [...document.querySelectorAll('[id="workbench.view.extension.dvicode"] .pane-header')].find(h => h.textContent.includes(${JSON.stringify(view)}) && h.getAttribute('aria-expanded') === 'true'); if (!h) return null; const b = h.getBoundingClientRect(); return { x: b.left + 60, y: b.top + b.height / 2 }; })()`);
  if (p) await click(p.x, p.y);
}
async function rightClick(x, y) {
  await moveTo(x, y);
  await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'right', clickCount: 1 });
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'right', clickCount: 1 });
  await sleep(400);
}
async function doubleClick(x, y) {
  await moveTo(x, y);
  for (const n of [1, 2]) {
    await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: n });
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: n });
  }
  await sleep(300);
}
/** Scrolls the active editor by dragging its scrollbar until line `line` is the first visible one. */
async function scrollTo(line) {
  const top = () => cdp.eval(`Math.min(...[...document.querySelectorAll('.editor-group-container.active .line-numbers')].map(e => +e.textContent).filter(n => n > 0))`);
  const sb = await cdp.eval(`(() => { const s = document.querySelector('.editor-group-container.active .editor-scrollable > .scrollbar.vertical > .slider'); const r = s.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + Math.min(r.height / 2, 20) }; })()`);
  await moveTo(sb.x, sb.y, 14);
  await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: sb.x, y: sb.y, button: 'left', clickCount: 1 });
  let y = sb.y;
  for (let i = 0; i < 400 && (await top()) < line; i++) {
    y += 2;
    await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: sb.x, y, button: 'left', buttons: 1 });
    await cdp.eval(`window.__ptr(${sb.x}, ${y})`);
    await sleep(12);
  }
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: sb.x, y, button: 'left', clickCount: 1 });
  mouse = { x: sb.x, y };
  await sleep(400);
}
/** Center of the first visible element matching `sel` whose text or label includes `text`. */
async function find(sel, text = '', ms = 6000) {
  return waitFor(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find(e => e.offsetParent && (e.textContent + ' ' + (e.getAttribute('aria-label') || '')).includes(${JSON.stringify(text)})); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.left + Math.min(r.width / 2, 60), y: r.top + r.height / 2 }; })()`, ms, `${sel} "${text}"`);
}
/** Clicks an item of the open context menu (VS Code draws it in a shadow root). */
async function menu(label) {
  const p = await waitFor(`(() => {
    const roots = [document, ...[...document.querySelectorAll('*')].filter(e => e.shadowRoot).map(e => e.shadowRoot)];
    for (const r of roots) {
      const it = [...r.querySelectorAll('[role="menuitem"], .action-menu-item, .action-item')].find(e => (e.textContent + ' ' + (e.getAttribute('aria-label') || '')).includes(${JSON.stringify(label)}) && e.getBoundingClientRect().height > 0);
      if (it) { const b = it.getBoundingClientRect(); return { x: b.left + 50, y: b.top + b.height / 2 }; }
    }
    return null;
  })()`, 6000, 'menu item ' + label);
  await click(p.x, p.y);
}
async function explorerOpen(...names) {
  for (const n of names) {
    const p = await find('.explorer-folders-view .monaco-list-row', n);
    await click(p.x, p.y);
    await sleep(350);
  }
}
/** Clicks a row of a DVICode view; with `inline`, its action button at the right instead. */
async function rowClick(view, label, inline = false) {
  const rows = await rowsOf(view);
  const r = rows.find((x) => x.label.startsWith(label));
  if (!r) throw new Error(`no row ${label} in ${view}`);
  if (inline) {
    const b = await cdp.eval(`(() => { const row = [...document.querySelectorAll('[id="workbench.view.extension.dvicode"] .monaco-list-row')].find(r => r.textContent.trim().startsWith(${JSON.stringify(label)})); const a = row && row.querySelector('.actions .action-label'); if (!a) return null; const b = a.getBoundingClientRect(); return { x: b.left + b.width / 2, y: b.top + b.height / 2 }; })()`);
    if (b) {
      await moveTo(r.x, r.y, 10);
      await sleep(250);
      return click(b.x, b.y);
    }
  }
  await click(r.x, r.y);
}
async function expandAll(view, max = 12) {
  for (let i = 0; i < max; i++) {
    const rows = await rowsOf(view);
    const c = rows.find((r) => r.expanded === 'false');
    if (!c) return;
    await click(c.x - 22 > 0 ? c.x - 22 : c.x, c.y);
    await sleep(350);
  }
}

// ------------------------------------------------------------------ recording

let frames = [];
let recording = false;
async function startRecording(dir) {
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  frames = [];
  recording = true;
  cdp.on('Page.screencastFrame', async (p) => {
    if (recording) {
      const f = path.join(dir, `f${String(frames.length).padStart(5, '0')}.jpg`);
      fs.writeFileSync(f, Buffer.from(p.data, 'base64'));
      frames.push({ f, t: p.metadata.timestamp });
    }
    cdp.send('Page.screencastFrameAck', { sessionId: p.sessionId }).catch(() => {});
  });
  await cdp.send('Page.startScreencast', { format: 'jpeg', quality: 92, maxWidth: W, maxHeight: H, everyNthFrame: 1 });
}
async function stopRecording(dir, hold = 1.5) {
  await sleep(hold * 1000);
  recording = false;
  await cdp.send('Page.stopScreencast');
  // A concat list with each frame's real duration.
  let list = '';
  for (let i = 0; i < frames.length; i++) {
    const d = i + 1 < frames.length ? frames[i + 1].t - frames[i].t : hold;
    list += `file '${frames[i].f.replace(/\\/g, '/')}'\nduration ${Math.max(d, 0.001).toFixed(3)}\n`;
  }
  list += `file '${frames[frames.length - 1].f.replace(/\\/g, '/')}'\n`;
  fs.writeFileSync(path.join(dir, 'list.txt'), list);
  return frames.length;
}

async function shot(file, scale = 1.5) {
  await cdp.eval('window.__ptrShow(false)');
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: scale, mobile: false });
  await sleep(700);
  const r = await cdp.send('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync(file, Buffer.from(r.data, 'base64'));
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: 1, mobile: false });
  await cdp.eval('window.__ptrShow(true)');
  await sleep(400);
  console.log('screenshot', file);
}

// ------------------------------------------------------------------ scenes

/** Mouse only: the Explorer, right-click menus, the back button and the hierarchy views;
 * with `edit`, a wrong type and a missing `;` typed into the code instead of Expand Macro. */
async function scenesMouse(edit) {
  const dir = path.join(out, 'frames');
  await closeAllEditors();
  await command('View: Show Explorer');
  await sleep(500);
  await startRecording(dir);
  await sleep(600);

  await caption('Open a file: DVICode compiles the testbench with UVM 1.2');
  await explorerOpen('tb', 'demo_env_pkg.sv');
  await sleep(700);
  await scrollTo(40);
  await sleep(500);

  await caption('Hover a UVM macro to see what it expands to');
  let p = await textPos('uvm_component_utils', 1);
  await moveTo(p.x, p.y);
  await waitFor(`!!document.querySelector('.monaco-hover:not(.hidden) .hover-contents, .monaco-resizable-hover')`, 6000, 'hover');
  await sleep(2400);
  await hideHover();

  await caption('Right-click › Go to Definition, into the UVM library');
  p = await textPos('uvm_env');
  await rightClick(p.x, p.y);
  await sleep(500);
  await menu('Go to Definition');
  await waitFor(`[...document.querySelectorAll('.tab.active .label-name')].some(e => e.textContent.includes('uvm_env'))`, 8000, 'uvm_env.svh');
  await sleep(2000);
  p = await find('.titlebar-container .action-label', 'Go Back');
  await click(p.x, p.y);
  await sleep(700);

  if (edit) {
    await caption('Change a type: the error shows as you type');
    p = await textPos('demo_cfg        cfg');
    await doubleClick(p.x - 40, p.y);
    await sleep(300);
    await type('demo_config', 90);
    await waitFor(`!!document.querySelector('.editor-group-container.active .squiggly-error')`, 6000, 'squiggle');
    await sleep(600);
    p = await textPos('demo_config');
    await moveTo(p.x, p.y + 2);
    await waitFor(`!!document.querySelector('.monaco-hover:not(.hidden) .hover-contents, .monaco-resizable-hover')`, 6000, 'error hover');
    await sleep(1800);
    await caption('…and the lightbulb fixes it');
    await click(p.x, p.y);
    await sleep(500);
    p = await find('.lightBulbWidget, .codicon-light-bulb, .codicon-lightbulb', '');
    await click(p.x, p.y);
    await sleep(500);
    p = await find('.action-widget .monaco-list-row', 'Did you mean');
    await sleep(500);
    await click(p.x, p.y);
    await sleep(1200);

    await caption('A missing ; is flagged at once');
    const end = await textPos('sb;');
    await click(end.x + 16, end.y);
    await key('End');
    await sleep(400);
    await key('Backspace');
    await waitFor(`!!document.querySelector('.editor-group-container.active .squiggly-error')`, 6000, 'syntax squiggle');
    await sleep(1800);
    await type(';', 90);
    await sleep(1400);
  } else {
    await caption('Right-click › Expand Macro: the code a `uvm_* macro generates');
    p = await textPos('uvm_component_utils', 1);
    await rightClick(p.x, p.y);
    await sleep(500);
    await menu('Expand Macro One Level');
    await waitFor(`document.querySelectorAll('.editor-group-container').length > 1`, 8000, 'expansion editor');
    await sleep(2800);
    await key('ctrl+alt+shift+F9');
    await sleep(500);
  }

  await caption('The UVM Verification Hierarchy of the test');
  p = await find('.activitybar .action-item .action-label', 'DVICode');
  await click(p.x, p.y);
  await sleep(900);
  await collapsePane('Compiled Files');
  await expandAll('Verification Hierarchy');
  await sleep(700);
  await caption('Click a component to jump to where it is created');
  await rowClick('Verification Hierarchy', 'mon');
  await sleep(1600);
  await rowClick('Verification Hierarchy', 'sb');
  await sleep(1600);
  await caption('…or to its class');
  await rowClick('Verification Hierarchy', 'drv', true);
  await sleep(1800);
  await caption('');
  const n = await stopRecording(dir, 1.2);
  console.log('frames', n);
}

async function scenesGif() {
  const dir = path.join(out, 'frames');
  await closeAllEditors();
  await command('View: Show Explorer');
  await sleep(500);
  await startRecording(dir);
  await sleep(600);

  await caption('A UVM testbench, compiled with UVM 1.2 as you open it');
  await openFile('demo_env_pkg.sv');
  await gotoLine(54, 3);
  await sleep(900);

  await caption('Hover: what a UVM macro expands to');
  let p = await textPos('uvm_component_utils', 1);
  await moveTo(p.x, p.y);
  await waitFor(`!!document.querySelector('.monaco-hover:not(.hidden) .hover-contents, .monaco-resizable-hover')`, 6000, 'hover');
  await sleep(2600);
  await hideHover();

  await caption('F12: go to definition, into the UVM library');
  p = await textPos('uvm_env');
  await click(p.x, p.y);
  await sleep(300);
  await key('F12');
  await waitFor(`[...document.querySelectorAll('.tab.active .label-name')].some(e => e.textContent.includes('uvm_env'))`, 8000, 'uvm_env.svh');
  await sleep(2200);
  await key('alt+ArrowLeft');
  await sleep(700);

  await caption('Expand Macro: the code a `uvm_* macro generates');
  p = await textPos('uvm_component_utils', 1);
  await click(p.x, p.y);
  await command('Expand Macro All Levels');
  await waitFor(`document.querySelectorAll('.editor-group-container').length > 1`, 8000, 'expansion editor');
  await sleep(3500);
  await command('View: Revert and Close Editor');

  await caption('UVM Verification Hierarchy of the selected test');
  await command('DVICode: Focus on Verification Hierarchy View');
  await sleep(900);
  await expandAll('Verification Hierarchy');
  await sleep(1800);

  await caption('');
  const n = await stopRecording(dir, 1.2);
  console.log('frames', n);
}

async function scenesShots() {
  // 1. Hover with the macro expansion.
  await closeAllEditors();
  await command('View: Show Explorer');
  await openFile('demo_env_pkg.sv');
  await gotoLine(54, 3);
  let p = await textPos('uvm_component_utils', 1);
  await moveTo(p.x, p.y);
  await waitFor(`!!document.querySelector('.monaco-hover:not(.hidden) .hover-contents, .monaco-resizable-hover')`, 6000, 'hover');
  await sleep(1200);
  await shot(path.join(out, 'hover.png'));
  await hideHover();

  // 2. Both hierarchies next to the environment class.
  await command('DVICode: Focus on Verification Hierarchy View');
  await sleep(800);
  await collapsePane('Compiled Files');
  await expandAll('Verification Hierarchy');
  await expandAll('Design Hierarchy');
  await gotoLine(54, 3);
  await moveTo(W - 40, H - 100, 4);
  await sleep(800);
  await shot(path.join(out, 'hierarchy.png'));

  // 3. Problems and a quick fix on the deliberate mistakes.
  await command('DVICode: Set Current Build Configuration');
  await pick('errors');
  await sleep(1500);
  await waitEngineReady();
  await closeAllEditors();
  await command('View: Show Explorer');
  await openFile('broken.sv');
  await command('View: Focus Problems');
  await sleep(1200);
  // The quick fix on the misspelled type.
  await gotoLine(7, 3);
  await moveTo(W / 2, 140, 6);
  await key('ctrl+.');
  await waitFor(`!!document.querySelector('.action-widget')`, 6000, 'quick fix menu');
  await sleep(800);
  await shot(path.join(out, 'problems.png'));
  await key('Escape');
  // Back to the default configuration.
  await command('DVICode: Set Current Build Configuration');
  await pick('default');
  await sleep(1000);
}

/** The GIF for the READMEs (about 2 MB) and an MP4 for posts. */
function encode(name = 'dvicode-demo') {
  const list = path.join(out, 'frames', 'list.txt');
  const ff = (args) => execFileSync('ffmpeg', ['-v', 'error', '-y', '-f', 'concat', '-safe', '0', '-i', list, ...args], { stdio: 'inherit' });
  ff(['-vf', 'fps=12,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle', '-loop', '0', path.join(out, name + '.gif')]);
  ff(['-vf', 'fps=30,scale=1280:-2:flags=lanczos,format=yuv420p', '-c:v', 'libx264', '-crf', '20', '-preset', 'slow', '-movflags', '+faststart', path.join(out, name + '.mp4')]);
  console.log('wrote', path.join(out, name + '.gif'), 'and .mp4');
}

// ------------------------------------------------------------------ main

prepareWorkspace();
prepareProfile();
const proc = spawn(CODE, [ws, '--user-data-dir', profile, '--extensions-dir', exts, `--remote-debugging-port=${PORT}`, '--new-window', '--skip-release-notes', '--skip-welcome', '--disable-workspace-trust'], { stdio: 'ignore', detached: false });
try {
  let target;
  for (let i = 0; i < 80 && !target; i++) {
    await sleep(500);
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      target = list.find((t) => t.type === 'page' && /workbench/.test(t.url));
    } catch {}
  }
  if (!target) throw new Error('no VS Code window');
  cdp = new Cdp(target.webSocketDebuggerUrl);
  await cdp.open();
  await cdp.send('Page.enable');
  await cdp.send('Runtime.enable');
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: 1, mobile: false });
  await waitFor(`!!document.querySelector('.monaco-workbench')`, 30000, 'workbench');
  await sleep(3000);
  await cdp.eval(OVERLAY);
  await command('Notifications: Clear All Notifications');
  await waitEngineReady();
  await sleep(1500);
  if (what === 'gif' || what === 'all') {
    await scenesGif();
    encode();
  }
  // Variants to compare: mouse only, and mouse with live editing.
  if (what === 'variants' || what === 'gif-mouse') {
    await scenesMouse(false);
    encode('dvicode-demo-mouse');
  }
  if (what === 'variants' || what === 'gif-edit') {
    await scenesMouse(true);
    encode('dvicode-demo-edit');
  }
  if (what === 'shots' || what === 'all') await scenesShots();
} catch (e) {
  console.error('FAILED:', e.message);
  if (cdp) {
    const r = await cdp.send('Page.captureScreenshot', { format: 'png' }).catch(() => null);
    if (r) fs.writeFileSync(path.join(out, 'failure.png'), Buffer.from(r.data, 'base64'));
  }
  process.exitCode = 1;
} finally {
  // The whole tree: the engine and VS Code's helpers keep files in the workspace open.
  try {
    execFileSync('taskkill', ['/pid', String(proc.pid), '/T', '/F'], { stdio: 'ignore' });
  } catch {
    proc.kill();
  }
}
