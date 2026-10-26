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
    if (!text.startsWith('tb_top(dut(u_fifo,g_mon[0](u_mon),g_mon[1](u_mon)))')) throw new Error(text);
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
    if (h.test !== 'demo_full_test' || text !== 'uvm_test_top(env(agents[i](mon(ap(→ env.sb.item_imp)),drv(seq_item_port(→ env.agents[i].sqr.seq_item_export)),sqr(seq_item_export(← env.agents[i].drv.seq_item_port))),sb(item_imp(← env.agents[i].mon.ap)),vsqr(seq_item_export)),extra_sb(item_imp))') {
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

  await test('verification hierarchy: overrides, objects, ports', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const c = await api.ready();
    const r = await c.sendRequest('dvicode/tree', { view: 'verification', test: 'demo_err_test', objects: true, ports: false, filter: null, reveal: null });
    const s = (n) => `${n.name}${n.children.length ? '(' + n.children.map(s).join(',') + ')' : ''}`;
    const text = s(r.roots[0]);
    if (text !== 'uvm_test_top(env(agents[i](mon,drv,sqr),sb,vsqr(demo_item_seq),cfg,regs,adapter),vseq(first,second))') throw new Error(text);
    const env = r.roots[0].children[0];
    const drv = env.children[0].children[1];
    const cond = JSON.stringify(drv.conditions);
    if (drv.class !== 'demo_err_driver' || drv.overrides[0].from !== 'demo_driver' || !drv.overrides[0].applied || cond !== '[["get_is_active() == UVM_ACTIVE"]]' || env.children[0].count !== '2') {
      throw new Error(JSON.stringify({ drv, count: env.children[0].count }));
    }
    const vseq = r.roots[0].children[1];
    if (vseq.starts[0].on !== 'uvm_test_top.env.vsqr' || vseq.children[0].starts[0].on !== 'uvm_test_top.env.vsqr') throw new Error(JSON.stringify(vseq));
    // The view's switch.
    const view = api.verification();
    view.test = 'demo_err_test';
    await vscode.commands.executeCommand('dvicode.verification.showObjects');
    try {
      const roots = await view.getChildren();
      const kids = (await view.getChildren(roots[0])).map((k) => `${k.label}:${k.description}`);
      if (kids.join(',') !== 'env:demo_env,vseq:demo_vseq · on env.vsqr') throw new Error(kids.join(','));
      if (!String(view.view.description).includes('objects')) throw new Error(`description: ${view.view.description}`);
      return `${text}; view: ${kids.join(', ')}`;
    } finally {
      await vscode.commands.executeCommand('dvicode.verification.hideObjects');
      view.test = undefined;
      view.refresh();
    }
  });

  await test('verification hierarchy: TLM connections of ports', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const view = api.verification();
    view.test = 'demo_full_test';
    await vscode.commands.executeCommand('dvicode.verification.showPorts');
    try {
      const child = async (item, name) => {
        const kids = await view.getChildren(item);
        const k = kids.find((x) => x.label === name);
        if (!k) throw new Error(`no ${name} under ${item ? item.label : 'the root'}: ${kids.map((x) => x.label).join(', ')}`);
        return k;
      };
      const root = (await view.getChildren())[0];
      const env = await child(root, 'env');
      const agents = await child(env, 'agents[i]');
      const ap = await child(await child(agents, 'mon'), 'ap');
      if (ap.description !== 'analysis port · → env.sb.item_imp') throw new Error(`ap: ${ap.description}`);
      const port = await child(await child(agents, 'drv'), 'seq_item_port');
      if (port.description !== 'seq_item_pull port · possibly → env.agents[i].sqr.seq_item_export') throw new Error(`seq_item_port: ${port.description}`);
      const imp = await child(await child(env, 'sb'), 'item_imp');
      const links = (await view.getChildren(imp)).map((k) => `${k.label}${k.description ? ` (${k.description})` : ''}`);
      if (imp.description !== 'analysis imp · 1 connection to it' || links.join(',') !== '← env.agents[i].mon.ap') throw new Error(`item_imp: ${imp.description}; ${links.join(',')}`);
      return `${ap.label}: ${ap.description}; ${port.label}: ${port.description}; ${imp.label}: ${imp.description}`;
    } finally {
      await vscode.commands.executeCommand('dvicode.verification.hidePorts');
      view.test = undefined;
      view.refresh();
    }
  });

  await test('structure view: groups, inherited members, follow the cursor', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const c = await api.ready();
    const doc = await open('tb/demo_env_pkg.sv');
    await vscode.window.showTextDocument(doc);
    // The request.
    const r = await c.sendRequest('dvicode/structure', { textDocument: { uri: api.toServer(doc.uri) } });
    const pkg = r.nodes.find((n) => n.name === 'demo_env_pkg');
    const classes = pkg.children.find((n) => n.name === 'Classes');
    const full = classes.children.find((n) => n.name === 'demo_full_test');
    const groups = full.children.map((g) => g.name).join(',');
    if (groups !== 'Fields,Methods,Inherited from demo_base_test,extends uvm_test') throw new Error(groups);
    const inherited = full.children[2].children.map((m) => `${m.name}${m.mark ? ` {${m.mark}}` : ''}`).join(',');
    if (!inherited.includes('build_phase {overridden in demo_full_test}')) throw new Error(inherited);
    // The view follows the cursor (on by default).
    await vscode.commands.executeCommand('dvicode.structure.focus');
    const active = await vscode.window.showTextDocument(doc);
    const s = api.structure();
    await waitFor('structure items', async () => s.getChildren().length > 0, 30000);
    const p = lineOf(doc, 'class demo_monitor').translate(4, 0);
    active.selection = new vscode.Selection(p, p);
    const sel = () => (s.view.selection[0] ? s.view.selection[0].label : '');
    await waitFor('structure selection', async () => sel() === 'new', 20000).catch((e) => {
      throw new Error(`${e.message}: selection '${sel()}', message '${s.view.message}'`);
    });
    return `${groups}; ${inherited}; selected ${sel()}`;
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
      if (design !== 'tb_top tb_top.dut tb_top.dut.g_mon[0] tb_top.dut.g_mon[0].u_mon tb_top.dut.g_mon[1] tb_top.dut.g_mon[1].u_mon') throw new Error(`design: ${design}`);
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

  await test('register model view: maps, registers, fields on demand', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const v = api.views().registers;
    await v.refresh();
    const roots = await v.getChildren();
    const nodes = [];
    const walk = async (items) => {
      for (const i of items) {
        nodes.push(`${i.placed.names.join('.')}${i.description ? ` (${i.description})` : ''}`);
        if (i.node.kind !== 'field') {
          await walk(await v.getChildren(i));
        }
      }
    };
    await walk(roots);
    const want = [
      'demo_regs',
      'demo_regs.bus (0x4000 · 4 bytes)',
      'demo_regs.bus.ctrl (0x4000 · 32 bits · RW)',
      'demo_regs.bus.ctrl.enable ([0] · RW · reset 0x0)',
      'demo_regs.bus.ctrl.mode ([5:4] · RW · reset 0x1)',
      'demo_regs.bus.status[0] (0x4010 · 32 bits · RO)',
      'demo_regs.bus.status[0].level ([7:0] · RO · reset 0x0)',
      'demo_regs.bus.status[1] (0x4014 · 32 bits · RO)',
      'demo_regs.bus.status[1].level ([7:0] · RO · reset 0x0)',
    ].join(' | ');
    if (nodes.join(' | ') !== want) throw new Error(`${nodes.join(' | ')}; message '${v.view && v.view.message}', last ${JSON.stringify(v.last)}`);
    return nodes.join(' | ');
  });

  await test('assertions and coverage view: through a bind and a covergroup argument', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const v = api.checks();
    const lines = () => {
      const out = [];
      const walk = (nodes, depth) => {
        for (const n of nodes) {
          const item = v.getTreeItem(n);
          out.push(`${'  '.repeat(depth)}${item.label}${item.description ? ` (${item.description})` : ''}`);
          walk(v.getChildren(n), depth + 1);
        }
      };
      walk(v.getChildren(), 0);
      return out.join(' | ');
    };
    const ask = async (file, needle) => {
      const doc = await open(file);
      const ed = await vscode.window.showTextDocument(doc);
      const p = lineOf(doc, needle).translate(0, needle.lastIndexOf(' ') + 1);
      ed.selection = new vscode.Selection(p, p);
      await vscode.commands.executeCommand('dvicode.showChecks');
      return lines();
    };
    const push = await ask('rtl/fifo.sv', 'input  logic         push');
    const wantPush = 'Through ports and arguments (1) |   push of u_sva (bind) |     p_no_overflow |       a_no_overflow';
    if (push !== wantPush) throw new Error(`push: ${push}; message '${v.view.message}'`);
    if (!v.view.description.startsWith('push (port of fifo)')) throw new Error(`description: ${v.view.description}`);
    const full = await ask('rtl/fifo.sv', 'output logic         full');
    const wantFull = 'Through ports and arguments (1) |   full of u_sva (bind) |     c_full |     p_no_overflow |       a_no_overflow';
    if (full !== wantFull) throw new Error(`full: ${full}`);
    const addr = await ask('tb/tb_pkg.sv', 'rand bit [7:0] addr');
    const wantAddr = 'Through ports and arguments (1) |   a of cg_addr.sample() |     cp_addr';
    if (addr !== wantAddr) throw new Error(`addr: ${addr}`);
    return `${push} || ${addr}`;
  });

  await test('parameters: values per instance in the hierarchy and in hover', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const c = await api.ready();
    const res = await c.sendRequest('dvicode/tree', { view: 'design', maxNodes: 1000 });
    const find = (nodes, name) => nodes.find((n) => n.name === name);
    const dut = find(res.roots[0].children, 'dut');
    const fifo = find(dut.children, 'u_fifo');
    const w = fifo.params.find((p) => p.name === 'W');
    if (!w || w.value !== '16' || w.origin !== 'override') throw new Error(JSON.stringify(fifo.params));
    const doc = await open('rtl/fifo.sv');
    const line = lineOf(doc, 'parameter int W');
    const pos = new vscode.Position(line.line, doc.lineAt(line.line).text.indexOf(' W ') + 1);
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', doc.uri, pos);
    const md = hovers.map((h) => h.contents.map((x) => (typeof x === 'string' ? x : x.value)).join('\n')).join('\n');
    if (!md.includes('- `16` in `tb_top.dut.u_fifo`') || !md.includes('`tb_top.dut.g_mon[1].u_mon`')) throw new Error(md);
    return `W=${w.value} (${w.origin}); ${md.split('\n').filter((l) => l.startsWith('- ')).join(' ')}`;
  });

  await test('format document: whitespace edits on an unsaved buffer', async () => {
    const doc = await open('rtl/fifo.sv');
    const ed = await vscode.window.showTextDocument(doc);
    const line = lineOf(doc, 'logic [AW:0]   count;').line;
    const lead = doc.lineAt(line).firstNonWhitespaceCharacterIndex;
    // Unindent one line and add trailing blanks to another (not saved).
    await ed.edit((e) => {
      e.delete(new vscode.Range(line, 0, line, lead));
      e.insert(doc.lineAt(line + 1).range.end, '   ');
    });
    try {
      const edits = await vscode.commands.executeCommand('vscode.executeFormatDocumentProvider', doc.uri, { tabSize: 2, insertSpaces: true });
      const got = (edits || []).map((x) => `${x.range.start.line - line}:${x.range.start.character}-${x.range.end.character}=${JSON.stringify(x.newText)}`).join(' ');
      const want = `0:0-0=${JSON.stringify(' '.repeat(lead))} 1:${doc.lineAt(line + 1).text.trimEnd().length}-${doc.lineAt(line + 1).text.length}=""`;
      if (got !== want) throw new Error(`edits: ${got}; want ${want}`);
      return got;
    } finally {
      await vscode.commands.executeCommand('workbench.action.files.revert');
    }
  });

  await test('drivers and loads view: through an output port and back', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const v = api.drivers();
    const lines = () => {
      const out = [];
      const walk = (nodes, depth) => {
        for (const n of nodes) {
          const item = v.getTreeItem(n);
          out.push(`${'  '.repeat(depth)}${item.label}${item.description ? ` (${item.description})` : ''}`);
          walk(v.getChildren(n), depth + 1);
        }
      };
      walk(v.getChildren(), 0);
      return out.join(' | ');
    };
    const doc = await open('rtl/top.sv');
    const ed = await vscode.window.showTextDocument(doc);
    const p = lineOf(doc, 'logic   full, empty').translate(0, 'logic   '.length);
    ed.selection = new vscode.Selection(p, p);
    await vscode.commands.executeCommand('dvicode.showDrivers');
    const drivers = lines();
    const wantDrivers = 'Through ports (1) |   full of u_fifo (output) |     assign full  = (count == DEPTH); (continuous · u_fifo)';
    if (drivers !== wantDrivers) throw new Error(`drivers: ${drivers}; message '${v.view.message}'`);
    if (v.view.description !== 'drivers of full (variable)') throw new Error(`description: ${v.view.description}`);
    // The view's button asks the other direction for the same signal.
    await vscode.commands.executeCommand('dvicode.accesses.showLoads');
    const loads = lines();
    const wantLoads = 'Reads (1) |   BUSY: if (full) state <= DONE; (read · in always_ff)';
    if (loads !== wantLoads) throw new Error(`loads: ${loads}; message '${v.view.message}'`);
    return `${drivers} || ${loads}`;
  });

  await test('compile context: include hover and the report', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const pkg = await open('tb/tb_pkg.sv');
    const at = lineOf(pkg, '`include "macros.svh"').translate(0, '  `include "'.length + 2);
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', pkg.uri, at);
    const hover = (hovers || []).map((h) => h.contents.map((c) => c.value ?? c).join(' ')).join(' ');
    if (!/macros\.svh/.test(hover) || !/Found in the folder of the including file/.test(hover)) throw new Error(`hover: ${hover}`);
    const header = await open('tb/macros.svh');
    const report = await vscode.workspace.openTextDocument(vscode.Uri.from({ scheme: 'dvicode-context', path: '/macros.svh (compile context).md', query: header.uri.toString() }));
    const text = report.getText();
    for (const want of ['Compiled in 1 context', 'tb_pkg.sv', '+define+DEMO_WIDTH=16', 'Include directories, in search order']) {
      if (!text.includes(want)) throw new Error(`report lacks '${want}': ${text.slice(0, 600)}`);
    }
    await vscode.window.showTextDocument(header);
    await vscode.commands.executeCommand('dvicode.showCompileContext');
    return `${hover.slice(0, 80)}; ${text.split('\n').find((l) => l.startsWith('1. ')) ?? ''}`;
  });

  await test('compile context: Compile This File As changes the problems of a shared header', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    const client = await api.ready();
    // Two files outside the build include one header: each open one compiles it (ad hoc), one
    // with X defined. Written for this test and removed after it.
    const dir = vscode.Uri.joinPath(folder, 'ctx_e2e');
    const write = (f, text) => vscode.workspace.fs.writeFile(vscode.Uri.joinPath(dir, f), Buffer.from(text));
    await write('h.svh', '`ifdef X\n  int only_x;\n`else\n  int only_nox;\n  initial only_nox = missing_nox;\n`endif\n');
    await write('x.sv', 'module mx;\n`include "h.svh"\nendmodule\n');
    await write('y.sv', '`define X\nmodule my;\n`include "h.svh"\nendmodule\n');
    try {
      await open('ctx_e2e/x.sv');
      await open('ctx_e2e/y.sv');
      const h = await open('ctx_e2e/h.svh');
      await vscode.window.showTextDocument(h);
      const undeclared = () => vscode.languages.getDiagnostics(h.uri).some((d) => d.message.includes('missing_nox'));
      // The open header's own unit (the default), then x.sv's and y.sv's by path.
      let last;
      const list = await waitFor('three contexts', async () => {
        last = await client.sendRequest('dvicode/compileContexts', { textDocument: { uri: api.toServer(h.uri) } });
        return last.total === 3 ? last : undefined;
      }, 30000).catch((e) => {
        throw new Error(`${e.message}: ${JSON.stringify(last)}`);
      });
      // X is not defined in the default context.
      await waitFor('the problem of the default context', async () => undeclared(), 30000);
      const y = list.items.find((i) => i.root.endsWith('/y.sv'));
      if (!y) throw new Error(JSON.stringify(list));
      const choice = { root: y.root, invocation_key: y.invocation_key, nth: y.nth, before: y.before };
      if (!(await vscode.commands.executeCommand('dvicode.selectCompileContext', h.uri, choice))) throw new Error('not chosen');
      await waitFor('no problem in y.sv\'s context', async () => !undeclared(), 30000);
      const report = await vscode.workspace.openTextDocument(vscode.Uri.from({ scheme: 'dvicode-context', path: '/h.svh (compile context).md', query: h.uri.toString() }));
      const md = report.getText();
      if (!/use the context chosen/.test(md) || !/y\.sv.*\*\*chosen\*\*/.test(md)) throw new Error(`report: ${md.slice(0, 800)}`);
      // A new engine gets the choice from the extension.
      await vscode.commands.executeCommand('dvicode.restartServer');
      const again = await api.ready();
      await waitFor('the choice in the new engine', async () => {
        const l = await again.sendRequest('dvicode/compileContexts', { textDocument: { uri: api.toServer(h.uri) }, limit: 0 });
        return l.state === 'chosen' && l.chosen && l.chosen.root.endsWith('/y.sv');
      }, 60000);
      await waitFor('no problem in y.sv\'s context after the restart', async () => !undeclared(), 30000);
      // Back to the default.
      await vscode.commands.executeCommand('dvicode.selectCompileContext', h.uri, null);
      await waitFor('the problem again', async () => undeclared(), 30000);
      return `${list.total} contexts; chosen ${y.root.split('/').pop()} (${y.ordinal} of ${list.total}), then the default`;
    } finally {
      await vscode.commands.executeCommand('workbench.action.closeAllEditors');
      await vscode.workspace.fs.delete(dir, { recursive: true, useTrash: false });
    }
  });

  await test('config_db: hover and go to definition from a get to its set', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const doc = await open('tb/demo_env_pkg.sv');
    const line = lineOf(doc, 'uvm_config_db #(demo_cfg)::get');
    const text = doc.lineAt(line.line).text;
    const onGet = new vscode.Position(line.line, text.indexOf('::get') + 3);
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', doc.uri, onGet);
    const md = hovers.map((h) => h.contents.map((c) => (typeof c === 'string' ? c : c.value)).join('\n')).join('\n');
    if (!md.includes('Candidate providers** (1)') || !md.includes('::set(this, "agent*", "cfg", cfg)')) throw new Error(md);
    const onField = new vscode.Position(line.line, text.indexOf('"cfg"') + 2);
    const defs = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', doc.uri, onField);
    const at = defs.map((d) => `${(d.uri || d.targetUri).path.split('/').pop()}:${(d.range || d.targetRange).start.line + 1}`).join(' ');
    const setLine = lineOf(doc, '::set(this, "agent*"').line + 1;
    if (at !== `demo_env_pkg.sv:${setLine}`) throw new Error(`definition: ${at}`);
    return `${md.split('\n')[0]} -> ${at}`;
  });

  await test('logs: links open a file from another machine and select a component', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    // A log as a farm machine writes it (its paths do not exist here). Removed after the test.
    const dir = vscode.Uri.joinPath(folder, 'logs_e2e');
    const log = vscode.Uri.joinPath(dir, 'sim.log');
    const text = [
      'UVM_INFO @ 0: reporter [RNTST] Running test demo_full_test...',
      'UVM_ERROR /nfs/farm/demo/tb/demo_env_pkg.sv(15) @ 100: uvm_test_top.env.agents[0].drv [DRV] boom',
      'Warning-[TMBIN] Too many bits in Based Number',
      '/nfs/farm/demo/tb/tb_pkg.sv, 3',
      '',
    ].join('\n');
    await vscode.workspace.fs.writeFile(log, Buffer.from(text));
    try {
      const doc = await vscode.workspace.openTextDocument(log);
      await vscode.window.showTextDocument(doc);
      const links = await waitFor('log links', async () => {
        const l = await vscode.commands.executeCommand('vscode.executeLinkProvider', doc.uri);
        const ours = (l || []).filter((x) => x.target && x.target.scheme === 'command' && x.target.path.startsWith('dvicode.logs.'));
        return ours.length >= 3 ? ours : undefined;
      });
      const args = (l) => JSON.parse(decodeURIComponent(l.target.query))[0];
      const shown = links.map((l) => doc.getText(l.range));
      const editorAt = () => {
        const ed = vscode.window.activeTextEditor;
        return ed ? `${vscode.workspace.asRelativePath(ed.document.uri)}:${ed.selection.active.line + 1}` : 'none';
      };
      // The UVM line's file, then the VCS message's location (the next line after its header).
      await vscode.commands.executeCommand('dvicode.logs.open', args(links[0]));
      const first = editorAt();
      if (first !== 'tb/demo_env_pkg.sv:15') throw new Error(`first link opened ${first}; links ${shown.join(' | ')}`);
      await vscode.commands.executeCommand('dvicode.logs.open', args(links[2]));
      const third = editorAt();
      if (third !== 'tb/tb_pkg.sv:3') throw new Error(`VCS location opened ${third}`);
      // The component: selected in the Verification Hierarchy, its class opened.
      await vscode.commands.executeCommand('dvicode.logs.component', args(links[1]));
      const cls = vscode.window.activeTextEditor;
      const line = cls ? cls.document.lineAt(cls.selection.active.line).text : '';
      if (!/class demo_driver/.test(line)) throw new Error(`component opened ${editorAt()}: ${line}`);
      return `${shown.join(' | ')} -> ${first}; ${third}; ${line.trim()}`;
    } finally {
      await vscode.commands.executeCommand('workbench.action.closeAllEditors');
      await vscode.workspace.fs.delete(dir, { recursive: true });
    }
  });

  await test('instance repair: port completion and connect missing ports', async () => {
    const doc = await open('rtl/top.sv');
    const ed = await vscode.window.showTextDocument(doc);
    const p = lineOf(doc, 'endmodule');
    try {
      await ed.edit((b) => b.insert(new vscode.Position(p.line, 0), '  fifo u_new (.clk(clk), .\n'));
      const at = new vscode.Position(p.line, '  fifo u_new (.clk(clk), .'.length);
      const labels = await waitFor('port completion', async () => {
        const list = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider', doc.uri, at);
        const names = list.items.map((i) => (typeof i.label === 'string' ? i.label : i.label.label));
        return names.includes('rst_n') ? names : undefined;
      }, 20000);
      // The connected port comes last.
      if (labels[labels.length - 1] !== 'clk' || !labels.includes('empty')) throw new Error(`completion: ${labels.join(' ')}`);
      // A complete statement: the action connects the rest.
      const line = doc.lineAt(p.line);
      await ed.edit((b) => b.replace(line.range, '  fifo u_new (.clk(clk));'));
      const inside = new vscode.Position(p.line, 8);
      const titles = await waitFor('connect action', async () => {
        const acts = await vscode.commands.executeCommand('vscode.executeCodeActionProvider', doc.uri, new vscode.Range(inside, inside), 'refactor');
        const t = (acts || []).map((x) => x.title);
        return t.some((x) => x.startsWith('Connect')) ? t : undefined;
      }, 20000);
      const want = 'Connect 7 missing ports of u_new';
      if (!titles.includes(want)) throw new Error(`actions: ${titles.join(' | ')}`);
      return `${labels.length} ports offered; ${want}`;
    } finally {
      await vscode.commands.executeCommand('workbench.action.files.revert');
    }
  });

  await test('factory override: hover says what it changes', async () => {
    const api = vscode.extensions.getExtension('hexk.dvicode').exports;
    await api.ready();
    const doc = await open('tb/demo_env_pkg.sv');
    const p = lineOf(doc, 'set_type_override_by_type(demo_driver');
    const at = p.translate(0, 4);
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', doc.uri, at);
    const md = hovers.map((h) => h.contents.map((c) => (typeof c === 'string' ? c : c.value)).join('\n')).join('\n');
    if (!md.includes('Factory type override `demo_driver` → `demo_err_driver`') || !md.includes('Changes')) throw new Error(md);
    return md.split('\n').filter((l) => l.startsWith('- ')).slice(0, 2).join('; ');
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
      if (first !== '1 of 3') throw new Error(`description: ${first}`);
      await vscode.commands.executeCommand('dvicode.design.nextOccurrence');
      await until('next occurrence', v.design, 'tb_top.dut.g_mon[0].u_mon');
      // Moving inside fifo keeps the occurrence shown.
      await at(fifo, 'module fifo', 4);
      await sleep(1500);
      if (sel(v.design) !== 'tb_top.dut.g_mon[0].u_mon') throw new Error(`moved to ${sel(v.design)}`);
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
      // Quick fixes with an edit: an editor may add its own (Cursor's "Fix in Agent" runs a command).
      return acts.filter((a) => a.kind && a.kind.value === 'quickfix' && a.edit);
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
