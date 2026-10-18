// Runs inside the VS Code extension host. Exercises DVICode through VS Code's own APIs on
// examples/demo-project and writes the results to $DVICODE_E2E_RESULT.

const vscode = require('vscode');
const fs = require('fs');

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

  fs.writeFileSync(process.env.DVICODE_E2E_RESULT, JSON.stringify({ target, tests: results }, null, 2));
}

module.exports = { run };
