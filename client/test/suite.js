// Runs inside the VS Code extension host. Exercises DVICode through VS Code's own APIs on
// examples/demo-project and writes the results to $DVICODE_E2E_RESULT (in a remote window,
// where the variable does not reach the extension host, to result.json next to this file).

const vscode = require('vscode');
const fs = require('fs');
const path = require('path');

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitFor(what, fn, timeoutMs = 90000) {
  const t0 = Date.now();
  let last;
  while (Date.now() - t0 < timeoutMs) {
    try {
      last = await fn();
      if (last) {
        return last;
      }
    } catch (e) {
      last = e;
    }
    await sleep(500);
  }
  throw new Error(`timed out waiting for ${what} (${last})`);
}

function lineOf(doc, needle, nth = 1) {
  let count = 0;
  for (let i = 0; i < doc.lineCount; i++) {
    const t = doc.lineAt(i).text;
    const c = t.indexOf(needle);
    if (c >= 0 && ++count === nth) {
      return new vscode.Position(i, c);
    }
  }
  throw new Error(`'${needle}' not found in ${doc.uri}`);
}

function rel(folder, uri) {
  const f = folder.toString();
  const u = uri.toString();
  return u.startsWith(f) ? u.slice(f.length + 1) : `(other root) ${u}`;
}

async function run() {
  const results = [];
  let target;
  const test = async (name, fn) => {
    try {
      const detail = await fn();
      results.push({ name, ok: true, detail: detail || '' });
    } catch (e) {
      results.push({ name, ok: false, detail: String(e && e.stack ? e.message : e) });
    }
  };
  const folder = vscode.workspace.workspaceFolders[0].uri;
  const open = async (p) => vscode.workspace.openTextDocument(vscode.Uri.joinPath(folder, p));

  await test('extension activates and the server starts', async () => {
    const ext = vscode.extensions.getExtension('hexk.dvicode');
    if (!ext) throw new Error('extension not found');
    const api = await ext.activate();
    await api.ready();
    target = api.target();
    return `server on ${target}; workspace ${folder.toString()}`;
  });

  await test('project builds (workspace symbol lookup)', async () => {
    const syms = await waitFor('workspace symbols', async () => {
      const s = await vscode.commands.executeCommand('vscode.executeWorkspaceSymbolProvider', 'fifo');
      return s && s.length ? s : undefined;
    });
    const fifo = syms.find((s) => s.name === 'fifo');
    if (!fifo) throw new Error(`no 'fifo' in ${syms.map((s) => s.name)}`);
    return `fifo -> ${rel(folder, fifo.location.uri)}:${fifo.location.range.start.line + 1}`;
  });

  await test('go to definition through a package import', async () => {
    const doc = await open('rtl/top.sv');
    const pos = lineOf(doc, 'state_e');
    const locs = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', doc.uri, pos);
    const l = locs[0];
    const where = `${rel(folder, l.uri || l.targetUri)}:${(l.range || l.targetRange).start.line + 1}`;
    if (where !== 'rtl/demo_pkg.sv:9') throw new Error(where);
    return where;
  });

  await test('go to definition of a class member through a variable', async () => {
    const doc = await open('tb/tb_top.sv');
    const p = lineOf(doc, 'drv.mbx.put');
    const locs = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', doc.uri, p.translate(0, 4));
    const l = locs[0];
    const where = `${rel(folder, l.uri || l.targetUri)}:${(l.range || l.targetRange).start.line + 1}`;
    if (where !== 'tb/tb_pkg.sv:39') throw new Error(where);
    return where;
  });

  await test('hover shows the type and its comment', async () => {
    const doc = await open('rtl/top.sv');
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', doc.uri, lineOf(doc, 'req_t'));
    const text = hovers.map((h) => h.contents.map((c) => (typeof c === 'string' ? c : c.value)).join('\n')).join('\n');
    if (!text.includes('typedef struct packed') || !text.includes('A bus request')) throw new Error(text);
    return text.split('\n')[1];
  });

  await test('completion lists class members (inherited too)', async () => {
    const doc = await open('tb/tb_top.sv');
    const p = lineOf(doc, 'randomize');
    const list = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider', doc.uri, p);
    const labels = list.items.map((i) => (typeof i.label === 'string' ? i.label : i.label.label));
    for (const want of ['convert2string', 'data', 'addr', 'print']) {
      if (!labels.includes(want)) throw new Error(`missing ${want} in ${labels.slice(0, 20)}`);
    }
    return `${labels.length} items`;
  });

  await test('find references of an enum item', async () => {
    const doc = await open('rtl/demo_pkg.sv');
    const refs = await vscode.commands.executeCommand('vscode.executeReferenceProvider', doc.uri, lineOf(doc, 'IDLE'));
    const where = refs.map((r) => `${rel(folder, r.uri)}:${r.range.start.line + 1}`).sort();
    if (where.length !== 3) throw new Error(where.join(', '));
    return where.join(', ');
  });

  await test('outline of a module', async () => {
    const doc = await open('rtl/fifo.sv');
    const syms = await vscode.commands.executeCommand('vscode.executeDocumentSymbolProvider', doc.uri);
    const fifo = syms.find((s) => s.name === 'fifo');
    if (!fifo) throw new Error('no fifo symbol');
    const kids = fifo.children.map((c) => c.name);
    for (const want of ['W', 'clk', 'mem', 'count']) {
      if (!kids.includes(want)) throw new Error(`missing ${want} in ${kids}`);
    }
    return kids.join(' ');
  });

  await test('problems in a file outside the build', async () => {
    const doc = await open('rtl/broken.sv');
    await vscode.window.showTextDocument(doc);
    const diags = await waitFor('diagnostics', async () => {
      const d = vscode.languages.getDiagnostics(doc.uri);
      return d.length >= 7 ? d : undefined;
    }, 30000);
    const codes = diags.map((d) => `${d.range.start.line + 1}:${typeof d.code === 'object' ? d.code.value : d.code}`).sort();
    return codes.join(' ');
  });

  await test('the build file is checked as it is typed, with hover', async () => {
    const doc = await open('.dvicode/default.build');
    if (doc.languageId !== 'dvicode-build') throw new Error(`language ${doc.languageId}`);
    const ed = await vscode.window.showTextDocument(doc);
    const p = lineOf(doc, 'rtl/fifo.sv');
    await ed.edit((b) => b.insert(new vscode.Position(p.line, 0), '-incdr rtl\nrtl/nofile.sv\n'));
    try {
      const diags = await waitFor('build file problems', async () => {
        const d = vscode.languages.getDiagnostics(doc.uri);
        return d.some((x) => x.message.includes('did you mean -incdir')) && d.some((x) => x.message.includes('nofile.sv')) ? d : undefined;
      }, 20000);
      const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', doc.uri, lineOf(doc, '$TB/tb_pkg.sv').translate(0, 5));
      const text = hovers.map((h) => h.contents.map((c) => (typeof c === 'string' ? c : c.value)).join('\n')).join('\n');
      if (!text.includes('tb_pkg.sv') || !text.includes('expands to')) throw new Error(`hover: ${text}`);
      return diags.map((d) => `${d.range.start.line + 1}: ${d.message}`).join('; ');
    } finally {
      await vscode.commands.executeCommand('workbench.action.files.revert');
    }
  });

  await test('edits are analysed incrementally', async () => {
    const doc = await open('rtl/top.sv');
    const ed = await vscode.window.showTextDocument(doc);
    const p = lineOf(doc, 'state_e state;');
    await ed.edit((b) => b.insert(new vscode.Position(p.line + 1, 0), '  logic new_signal;\n'));
    await ed.edit((b) => b.insert(new vscode.Position(p.line + 2, 0), '  assign new_signal = typo_signal;\n'));
    const d = await waitFor('diagnostic for typo_signal', async () => {
      const ds = vscode.languages.getDiagnostics(doc.uri).filter((x) => x.message.includes('typo_signal'));
      return ds.length ? ds : undefined;
    }, 20000).catch(() => undefined);
    // `assign` left-hand sides may be implicit nets, so the check is go-to-definition instead.
    const locs = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', doc.uri, lineOf(doc, 'new_signal ='));
    await vscode.commands.executeCommand('workbench.action.files.revert');
    if (!locs.length || (locs[0].range || locs[0].targetRange).start.line !== p.line + 1) throw new Error(`definition: ${JSON.stringify(locs)}`);
    return `new declaration found${d ? '; typo reported' : ''}`;
  });

  await test('design hierarchy request', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const c = await api.ready();
    const h = await c.sendRequest('dvicode/designHierarchy', { top: null });
    const s = (n) => `${n.name}${n.children.length ? '(' + n.children.map(s).join(',') + ')' : ''}`;
    const text = h.map(s).join(' ');
    if (!text.startsWith('tb_top(dut(u_fifo,g_mon(u_mon)))')) throw new Error(text);
    return text;
  });

  await test('expand macro', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const c = await api.ready();
    const doc = await open('tb/tb_pkg.sv');
    const p = lineOf(doc, '`CHECK');
    const r = await c.sendRequest('dvicode/expandMacro', { textDocument: { uri: api.toServer(doc.uri) }, position: p.translate(0, 2), all: false });
    if (!r || !r.text.includes('$error')) throw new Error(JSON.stringify(r));
    return r.text;
  });

  await test('verification hierarchy request and view', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const c = await api.ready();
    const h = await c.sendRequest('dvicode/verificationHierarchy', { test: null });
    const s = (n) => `${n.name}${n.children.length ? '(' + n.children.map(s).join(',') + ')' : ''}`;
    const text = h.roots.map(s).join(' ');
    if (h.test !== 'demo_full_test' || text !== 'uvm_test_top(env(agents[i](mon(ap),drv,sqr),sb),extra_sb)') {
      throw new Error(`${h.test}: ${text}`);
    }
    // The tree view, with another test picked.
    const view = api.verification();
    view.test = 'demo_smoke_test';
    try {
      const roots = await view.getChildren();
      const kids = await view.getChildren(roots[0]);
      const got = `${roots[0].label}:${roots[0].description} -> ${kids.map((k) => `${k.label}:${k.description}`).join(',')}`;
      if (got !== 'uvm_test_top:demo_smoke_test -> env:demo_env') throw new Error(got);
      return `${text}; view: ${got}`;
    } finally {
      view.test = undefined;
      view.refresh();
    }
  });

  /** Names of every node of a view, depth first. */
  const allNodes = async (view) => {
    const out = [];
    const walk = async (items) => {
      for (const i of items) {
        out.push(i.placed.names.join('.'));
        await walk(await view.getChildren(i));
      }
    };
    await walk(await view.getChildren());
    return out;
  };

  await test('views: filter', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const v = api.views();
    try {
      await v.design.setFilter('mon');
      const design = (await allNodes(v.design)).join(' ');
      if (design !== 'tb_top tb_top.dut tb_top.dut.g_mon tb_top.dut.g_mon.u_mon') throw new Error(`design: ${design}`);
      await v.verification.setFilter('drv');
      const verif = (await allNodes(v.verification)).join(' ');
      if (verif !== 'demo_full_test demo_full_test.env demo_full_test.env.agents[i] demo_full_test.env.agents[i].drv') throw new Error(`verification: ${verif}`);
      await v.files.setFilter('*_pkg.sv');
      const files = (await v.files.getChildren()).map((i) => i.node.path.replace(/\\/g, '/').split('/').pop()).sort().join(' ');
      if (files !== 'demo_env_pkg.sv demo_pkg.sv tb_pkg.sv uvm_pkg.sv') throw new Error(`files: ${files}`);
      await v.design.setFilter('nothing_here');
      if (v.design.view.message !== "Nothing matches 'nothing_here'.") throw new Error(`message: ${v.design.view.message}`);
      return `${design} | ${verif} | ${files}`;
    } finally {
      for (const x of [v.design, v.verification, v.files]) {
        await x.setFilter(undefined);
      }
    }
  });

  await test('views: follow the editor', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const v = api.views();
    const sel = (view) => (view.view.selection[0] ? view.view.selection[0].placed.names.join('.') : '');
    // On a timeout: what the view shows.
    const state = (view) => `selection '${sel(view)}', description '${view.view.description}', message '${view.view.message}', visible ${view.view.visible}, editor ${vscode.window.activeTextEditor && vscode.window.activeTextEditor.document.uri}`;
    const until = (what, view, want) =>
      waitFor(what, async () => sel(view) === want, 20000).catch((e) => {
        throw new Error(`${e.message}: ${state(view)}`);
      });
    const at = async (doc, needle, down) => {
      const ed = await vscode.window.showTextDocument(doc);
      const p = lineOf(doc, needle).translate(down, 0);
      ed.selection = new vscode.Selection(p, p);
      return ed;
    };
    await vscode.commands.executeCommand('dvicode.designHierarchy.focus');
    await vscode.commands.executeCommand('dvicode.design.syncOn');
    try {
      // In fifo: its first instance; the editor keeps the focus.
      const fifo = await open('rtl/fifo.sv');
      await at(fifo, 'module fifo', 3);
      await until('design selection', v.design, 'tb_top.dut.u_fifo');
      if (vscode.window.activeTextEditor.document !== fifo) throw new Error('the editor lost the focus');
      const first = v.design.view.description;
      if (first !== '1 of 2') throw new Error(`description: ${first}`);
      await vscode.commands.executeCommand('dvicode.design.nextOccurrence');
      await until('next occurrence', v.design, 'tb_top.dut.g_mon.u_mon');
      // Moving inside fifo keeps the occurrence shown.
      await at(fifo, 'module fifo', 4);
      await sleep(1500);
      if (sel(v.design) !== 'tb_top.dut.g_mon.u_mon') throw new Error(`moved to ${sel(v.design)}`);
      // On an instantiation line: that instance.
      const top = await open('rtl/top.sv');
      await at(top, 'u_fifo (', 0);
      await until('instance selection', v.design, 'tb_top.dut.u_fifo');
      // The verification hierarchy follows classes.
      await vscode.commands.executeCommand('dvicode.verificationHierarchy.focus');
      await vscode.commands.executeCommand('dvicode.verification.syncOn');
      const env = await open('tb/demo_env_pkg.sv');
      await at(env, 'class demo_monitor', 2);
      await until('verification selection', v.verification, 'demo_full_test.env.agents[i].mon');
      return `${first} -> ${sel(v.design)}; ${sel(v.verification)}`;
    } finally {
      await vscode.commands.executeCommand('dvicode.design.syncOff');
      await vscode.commands.executeCommand('dvicode.verification.syncOff');
    }
  });

  await test('call hierarchy: incoming and outgoing calls', async () => {
    const doc = await open('tb/tb_pkg.sv');
    const pos = lineOf(doc, 'extern function void print').translate(0, 'extern function void '.length);
    const items = await vscode.commands.executeCommand('vscode.prepareCallHierarchy', doc.uri, pos);
    if (!items || items.length !== 1 || items[0].name !== 'print') throw new Error(JSON.stringify(items));
    const inc = await vscode.commands.executeCommand('vscode.provideIncomingCalls', items[0]);
    const incText = inc.map((x) => `${x.from.name}:${x.fromRanges[0].start.line + 1}`).join(',');
    if (incText !== 'run:50') throw new Error(`incoming ${incText}`);
    const out = await vscode.commands.executeCommand('vscode.provideOutgoingCalls', items[0]);
    const outText = out.map((x) => x.to.name).join(',');
    if (outText !== 'convert2string') throw new Error(`outgoing ${outText}`);
    // One level further up: run is called from the tb_top module.
    const up = await vscode.commands.executeCommand('vscode.provideIncomingCalls', inc[0].from);
    const upText = up.map((x) => `${x.from.name}:${rel(folder, x.from.uri)}:${x.fromRanges[0].start.line + 1}`).join(',');
    if (upText !== 'tb_top:tb/tb_top.sv:20') throw new Error(`run incoming ${upText}`);
    return `print <- ${incText} <- ${upText}; print -> ${outText}`;
  });

  await test('quick fixes: did you mean, import package, declare variable', async () => {
    const doc = await open('rtl/broken.sv');
    const ed = await vscode.window.showTextDocument(doc);
    await waitFor('diagnostics', async () => vscode.languages.getDiagnostics(doc.uri).length >= 7, 30000);
    const fixesAt = async (line, needle) => {
      const p = lineOf(doc, needle, 1);
      if (p.line !== line - 1) throw new Error(`'${needle}' is on line ${p.line + 1}`);
      const acts = await vscode.commands.executeCommand('vscode.executeCodeActionProvider', doc.uri, new vscode.Range(p, p.translate(0, needle.length)));
      return acts.filter((a) => a.kind && a.kind.value === 'quickfix');
    };
    const typo = await fixesAt(7, 'stat_e');
    const imp = await fixesAt(25, 'req_t r;');
    const decl = await fixesAt(12, 'undeclared_var');
    const titles = [...typo, ...imp, ...decl].map((a) => a.title);
    const want = ["Did you mean 'state_e'?", "Import package 'demo_pkg' (demo_pkg::*)", "Declare variable 'undeclared_var'"];
    if (JSON.stringify(titles) !== JSON.stringify(want)) throw new Error(titles.join(' | '));
    // Apply the import and check the text, then undo.
    if (!(await vscode.workspace.applyEdit(imp[0].edit))) throw new Error('edit not applied');
    const l = lineOf(doc, 'module broken3;').line;
    const added = doc.lineAt(l + 1).text;
    await vscode.window.showTextDocument(doc);
    await vscode.commands.executeCommand('workbench.action.files.revert');
    if (added !== '  import demo_pkg::*;') throw new Error(`line after module broken3: '${added}'`);
    void ed;
    return titles.join(' | ');
  });

  await test('internal error: notice, issue text, engine keeps working', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const c = await api.ready();
    let failed = '';
    try {
      await c.sendRequest('dvicode/debug/fail', {});
    } catch (e) {
      failed = e.message;
    }
    if (!failed.includes('failed')) throw new Error(`request did not fail: '${failed}'`);
    await waitFor('internal error notice', async () => api.lastInternalError() !== undefined, 10000);
    const e = api.lastInternalError();
    if (e.feature !== 'dvicode/debug/fail' || !e.location.includes('server.rs')) throw new Error(JSON.stringify(e));
    const body = api.issueBody();
    for (const want of ['### Setup', 'test failure requested by the client', 'Backtrace', 'Log (last']) {
      if (!body.includes(want)) throw new Error(`issue text lacks '${want}'`);
    }
    // The engine still answers.
    const h = await c.sendRequest('dvicode/designTops');
    if (!h.includes('tb_top')) throw new Error(`after the failure: ${JSON.stringify(h)}`);
    return `${e.feature} at ${e.location}; issue text ${body.length} chars`;
  });

  const host = `${vscode.env.remoteName || 'local'} ${process.platform}-${process.arch}`;
  const out = process.env.DVICODE_E2E_RESULT || path.join(__dirname, 'result.json');
  fs.writeFileSync(out, JSON.stringify({ target, host, tests: results }, null, 2));
}

module.exports = { run };
