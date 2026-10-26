// Unit tests of src/viewmodel.ts: item keys, the occurrences of the editor's position, "k of N"
// cycling and the texts of the views.

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');

const vm = require(path.join(__dirname, '..', '..', 'out', 'unit', 'viewmodel.js'));

/** A design node. */
const d = (name, definition, children = [], kind = 'module') => ({ name, definition, kind, children });

// top: c0 (core: u_rx, u_rx), c1 (core: u_rx), g (generate: f (fifo)).
const design = [
  d('top', 'top', [
    d('c0', 'core', [d('u_rx', 'fifo'), d('u_rx', 'fifo')]),
    d('c1', 'core', [d('u_rx', 'fifo')]),
    d('g', '', [d('f', 'fifo')], 'generate'),
  ]),
];

test.describe('compile context report', () => {
  const ctx = {
    uri: 'file:///p/shared/common.svh',
    known: true,
    auto: false,
    build_file: 'file:///p/.dvicode/default.build',
    contexts: [
      { invocation: 0, mode: 'default', root_role: 'top', root: 'file:///p/top_a.sv', order: [1, 2], is_root: false, chain: [{ uri: 'file:///p/top_a.sv', line: 1 }], chain_cut: false },
      { invocation: 1, mode: 'second', root_role: 'top', root: 'file:///p/top_b.sv', order: [1, 1], is_root: false, chain: [{ uri: 'file:///p/top_b.sv', line: 2 }], chain_cut: false },
    ],
    contexts_total: 3,
    skip_matches: [],
    detail: {
      invocation: { index: 0, mode: 'default', auto: false, top_files: 2, defines: [['MODE_A', null]], defines_total: 1, undefines: [], incdirs: ['/p/shared'], incdirs_total: 1, lib_dirs: 0, lib_files: 0 },
      macros: [
        { name: 'MODE_A', used: false, tested: true, undefined: false, seen: [{ defined: true, at: null, command_line: true, params: null, value: '', count: 1 }], states_cut: 0 },
        { name: 'Y', used: true, tested: false, undefined: true, seen: [{ defined: true, at: { uri: 'file:///p/a.sv', line: 0 }, command_line: false, params: '(x)', value: 'x+1', count: 2 }, { defined: false, at: null, command_line: false, params: null, value: '', count: 1 }], states_cut: 3 },
      ],
      macros_total: 1,
      includes: [{ line: 6, text: 'leaf.svh', resolved: 'file:///p/shared/leaf.svh', entered: true, step: 'folder', incdir: null, shadowed: ['file:///p/leaf.svh'] }],
      includes_total: 1,
      probes_cut: false,
      inactive_lines: 2,
      duplicates: [{ kind: 'module', name: 'fifo`x', here: { uri: 'file:///p/top_a.sv', line: 3 }, others: [{ uri: 'file:///p/other.sv', line: 0 }], others_total: 1, default: { uri: 'file:///p/top_a.sv', line: 3 }, rule: 'first compiled' }],
      duplicates_cut: false,
      problem_counts: [['INCLUDE_NOT_FOUND', 1]],
      problems_capped: false,
      preprocessing: [{ code: 'INCLUDE_NOT_FOUND', line: 7, message: 'Cannot find include file "missing_<x>.svh"' }],
    },
  };
  const md = vm.compileContextMarkdown(ctx, (u, line) => `<${u}${line === undefined ? '' : `#L${line + 1}`}>`, (u) => u.replace('file:///p/', ''));
  test.it('lists the contexts with their chains and says which one the features use', () => {
    assert.match(md, /## Compiled in 3 contexts/);
    assert.match(md, /use the first one/);
    assert.match(md, /1\. From \[top\\_a\.sv\]\(<file:\/\/\/p\/top_a\.sv>\) \(top file, 1 of 2 top files\) in invocation 1: \[top\\_a\.sv:2\]\(<file:\/\/\/p\/top_a\.sv#L2>\)/);
    assert.match(md, /in invocation 2 \(second\)/);
    assert.match(md, /3\. … and 1 more/);
  });
  test.it('shows the first context: macros, includes, removed lines, duplicates, problems', () => {
    assert.match(md, /`MODE_A` \(tested\): defined on the command line/);
    assert.match(md, /`Y` \(used, `undef\): `\(x\)` defined at \[a\.sv:1\]\(<file:\/\/\/p\/a\.sv#L1>\) as `x\+1` \(at 2 places\); not defined \(at 1 place\); … 3 more/);
    assert.match(md, /found in this file's folder/);
    assert.match(md, /Also there today, later in the search: \[leaf\.svh\]/);
    assert.match(md, /2 lines are in regions/);
    assert.match(md, /module `` fifo`x ``: also at \[other\.sv:1\]/);
    assert.match(md, /gets this one by default \(first compiled\)/);
    // Source text is escaped.
    assert.match(md, /missing\\_\\<x\\>\.svh/);
  });
  test.it('says plainly when a file is not compiled', () => {
    const none = vm.compileContextMarkdown({ ...ctx, known: false, contexts: [], contexts_total: 0, detail: null, skip_matches: [[0, '*gen*']] }, (u) => u, (u) => u);
    assert.match(none, /\*\*Not compiled\.\*\* The engine has not read this file/);
    assert.match(none, /Matches `\+dvicode_skip_compile\+\*gen\*` of invocation 1/);
  });
});

test.describe('placeTree', () => {
  const t = vm.placeTree(design, (n) => n.name);
  test.it('gives repeated sibling names their own keys and keeps the names', () => {
    const keys = vm.depthFirst(t.roots).map((p) => p.key.split('\u0000').join('/'));
    assert.deepEqual(keys, ['top', 'top/c0', 'top/c0/u_rx', 'top/c0/u_rx#2', 'top/c1', 'top/c1/u_rx', 'top/g', 'top/g/f']);
    assert.equal(t.byNames.get(vm.namesKey(['top', 'c0', 'u_rx'])).length, 2);
  });
  test.it('does not give a repeated name the key of a sibling named like it', () => {
    const v = vm.placeTree([d('r', 'x'), d('r', 'x'), d('r#2', 'x'), d('r#3', 'x'), d('r', 'x')], (n) => n.name);
    const keys = v.roots.map((p) => p.key);
    assert.deepEqual(keys, ['r', 'r#2', 'r#2#2', 'r#3', 'r#4']);
    assert.equal(v.byKey.size, 5);
  });
  test.it('links parents and notices cut nodes', () => {
    const f = t.byNames.get(vm.namesKey(['top', 'g', 'f']))[0];
    assert.deepEqual(f.parent.names, ['top', 'g']);
    assert.equal(t.truncated, false);
    assert.equal(vm.placeTree([{ ...d('x', 'x'), truncated: true }], (n) => n.name).truncated, true);
  });
});

test.describe('occurrences', () => {
  const t = vm.placeTree(design, (n) => n.name);
  test.it('finds instances of a module in view order', () => {
    const got = vm.occurrences(t, (p) => vm.isDesignTarget(p, { definition: 'fifo' }));
    assert.deepEqual(got.map((n) => n.join('.')), ['top.c0.u_rx', 'top.c0.u_rx', 'top.c1.u_rx', 'top.g.f']);
  });
  test.it('finds one instance in a module, through generate blocks', () => {
    assert.equal(vm.occurrences(t, (p) => vm.isDesignTarget(p, { definition: 'core', instance: 'u_rx' })).length, 3);
    assert.equal(vm.occurrences(t, (p) => vm.isDesignTarget(p, { definition: 'top', instance: 'f' })).length, 1);
    assert.equal(vm.occurrences(t, (p) => vm.isDesignTarget(p, { definition: 'core', instance: 'f' })).length, 0);
  });
  test.it('finds components of a class', () => {
    const v = vm.placeTree([{ name: 'uvm_test_top', class: 't1', children: [{ name: 'a0', class: 'agent', children: [] }, { name: 'a1', class: 'agent', children: [] }] }], (n, root) => (root ? n.class : n.name));
    assert.deepEqual(vm.occurrences(v, (p) => vm.isClassTarget(p, { class: 'agent' })), [['t1', 'a0'], ['t1', 'a1']]);
  });
});

test.describe('Cycle', () => {
  const paths = [['a'], ['b'], ['c']];
  test.it('starts at the first, steps around, and keeps its place while the target is the same', () => {
    const c = new vm.Cycle();
    assert.equal(c.update('T', paths, undefined), true);
    assert.deepEqual(c.current(), ['a']);
    assert.deepEqual(c.step(1), ['b']);
    assert.deepEqual(c.step(-2), ['c']);
    // Moving the cursor inside the same module: still c.
    assert.equal(c.update('T', paths, undefined), false);
    assert.deepEqual(c.current(), ['c']);
    // A new target starts at its first occurrence.
    c.update('U', paths, undefined);
    assert.deepEqual(c.current(), ['a']);
  });
  test.it('prefers the node selected in the view', () => {
    const c = new vm.Cycle();
    c.update('T', paths, ['b']);
    assert.deepEqual(c.current(), ['b']);
  });
  test.it('follows its occurrence when others come and go', () => {
    const c = new vm.Cycle();
    c.update('T', paths, undefined);
    c.step(2);
    c.update('T', [['x'], ...paths], undefined);
    assert.deepEqual(c.current(), ['c']);
    c.update('T', [['a']], undefined);
    assert.deepEqual(c.current(), ['a']);
  });
});

test.describe('view texts', () => {
  const base = { connected: true, empty: false };
  test.it('describes the scope, the filter, the limits and the position', () => {
    assert.equal(vm.viewDescription({ ...base, scope: 'top tb' }), 'top tb');
    assert.equal(vm.viewDescription({ ...base, filter: 'fifo', matches: 1 }), "filter 'fifo': 1 match");
    assert.equal(vm.viewDescription({ ...base, filter: 'f', matches: 5000, incomplete: 'matches' }), "filter 'f': 5,000 matches · first 5,000 matches");
    assert.equal(vm.viewDescription({ ...base, position: { index: 1, count: 3 } }), '2 of 3');
    assert.equal(vm.viewDescription({ ...base, position: { index: 0, count: 5000, more: true } }), '1 of 5000+');
    assert.equal(vm.viewDescription({ ...base, position: { index: 0, count: 1 } }), undefined);
    // The node limit is shown on the nodes (…), not in the description.
    assert.equal(vm.viewDescription({ ...base, incomplete: 'nodes' }), undefined);
    assert.equal(vm.viewDescription({ ...base, scope: 'top tb', stale: true }), 'top tb · out of date: Refresh');
  });
  test.it('shows the first message that applies', () => {
    assert.equal(vm.viewMessage({ ...base, connected: false, error: 'x' }), 'The DVICode server is not running.');
    assert.equal(vm.viewMessage({ ...base, error: 'boom', filter: 'f', empty: true }), 'Could not get the tree: boom');
    assert.equal(vm.viewMessage({ ...base, filter: 'zz', empty: true, emptyText: 'No test' }), "Nothing matches 'zz'.");
    assert.equal(vm.viewMessage({ ...base, empty: true, emptyText: 'No test' }), 'No test');
    assert.equal(vm.viewMessage({ ...base, syncNote: 'not here' }), 'not here');
    assert.equal(vm.viewMessage(base), undefined);
  });
});

test.describe('Verification Hierarchy nodes', () => {
  const base = { name: 'drv', path: 'uvm_test_top.env.agt.drv', class: 'err_driver', kind: 'driver' };
  test.it('describes overrides, conditions, loops and alternatives', () => {
    const n = {
      ...base,
      count: 'up to 4',
      conditions: [['cfg.is_active']],
      alternatives: [{ class: 'b_driver', conditions: [['!(cfg.is_active)']] }],
      overrides: [
        { from: 'my_driver', to: 'err_driver', instance: false, set_in: 'err_test::build_phase', applied: true },
        { from: 'err_driver', to: 'worse_driver', instance: true, path: 'uvm_test_top.*', set_in: 'err_test::build_phase', conditions: [['inject']], applied: false },
      ],
    };
    assert.equal(vm.verifDescription(n), 'err_driver ×≤4 (override) · may be overridden · conditional · or b_driver');
    const tip = vm.verifTooltip(n).split('\n\n');
    assert.deepEqual(tip, [
      '**drv** : `err_driver` (driver)',
      'uvm_test_top.env.agt.drv',
      'Override: `my_driver` → `err_driver`: type override in `err_test::build_phase`',
      'May be overridden: `err_driver` → `worse_driver`: instance override (uvm_test_top.*) in `err_test::build_phase`, when `inject`',
      'Created only when `cfg.is_active`.',
      'Or `b_driver` when `!(cfg.is_active)`.',
      'Created in a loop of up to 4 iterations (it can stop early).',
    ]);
  });
  test.it('describes sequences and where they start', () => {
    const one = { name: 's', path: 'uvm_test_top.s', class: 'seq', kind: 'sequence', object: true, starts: [{ on: 'uvm_test_top.env.sqr', path: true, phase: 'main_phase' }] };
    assert.equal(vm.verifDescription(one), 'seq · on env.sqr');
    assert.equal(vm.verifTooltip(one), '**s** : `seq` (sequence)\n\nStarted on uvm_test_top.env.sqr in `main_phase`.');
    const two = { ...one, starts: [...one.starts, { on: 'p_sequencer.x', path: false, phase: 'body' }] };
    assert.equal(vm.verifDescription(two), 'seq · on 2 sequencers');
    assert.equal(vm.verifDescription({ ...one, starts: [] }), 'seq · not started here');
    assert.equal(vm.verifDescription({ ...base, ports_hidden: 3, truncated: true }), 'err_driver …');
    // An endpoint shows what it leads to; a connection its notes, and its tooltip who makes it.
    const port = { name: 'ap', path: 'uvm_test_top.env.mon.ap', class: 'uvm_analysis_port', kind: 'port', connection: 'analysis port · → env.sb.imp' };
    assert.equal(vm.verifDescription(port), 'analysis port · → env.sb.imp');
    assert.match(vm.verifTooltip(port), /Analysis port · → env\.sb\.imp\./);
    const link = { name: '✕ ← e.pull', path: 'uvm_test_top.e', class: 'seq_item_pull and analysis are different interfaces', kind: 'connection' };
    assert.equal(vm.verifDescription(link), 'seq_item_pull and analysis are different interfaces');
    assert.match(vm.verifTooltip(link), /Made by `uvm_test_top\.e`/);
  });
  test.it('finds a class by the class built, or the class an override replaced', () => {
    const p = (node) => ({ node, parent: undefined, children: [], names: [], key: '' });
    assert.ok(vm.isClassTarget(p({ class: 'a', def_uri: 'file:///a.sv' }), { class: 'a', classUri: 'file:///a.sv' }));
    assert.ok(!vm.isClassTarget(p({ class: 'a', def_uri: 'file:///other.sv' }), { class: 'a', classUri: 'file:///a.sv' }));
    assert.ok(vm.isClassTarget(p({ class: 'b', overrides: [{ from: 'a', to: 'b', applied: true }] }), { class: 'a' }));
    assert.ok(!vm.isClassTarget(p({ class: 'a', overrides: [{ from: 'a', to: 'b', applied: false }] }), { class: 'b' }));
  });
});

test.describe('Structure', () => {
  const at = (sl, sc, el, ec, uri = 'file:///a.sv') => ({ uri, range: { start: { line: sl, character: sc }, end: { line: el, character: ec } } });
  const n = (id, kind, extent, children = [], extra = {}) => ({ id, name: id, kind, extent, children, ...extra });
  const tree = [
    n('top', 'module', at(0, 0, 20, 9), [
      n('Signals', 'group', undefined, [n('a', 'signal', at(1, 2, 1, 10))]),
      n('Processes', 'group', undefined, [n('always_ff', 'process', at(3, 2, 6, 5))]),
    ]),
    n('c', 'class', at(22, 0, 40, 8), [n('Inherited from base', 'group', undefined, [n('x', 'field', at(2, 0, 2, 5), [], { inherited_from: 'base' })])]),
  ];
  test.it('finds the innermost node at the cursor, through groups', () => {
    assert.deepEqual(vm.innermostAt(tree, (u) => u === 'file:///a.sv', 4, 0).map((x) => x.id), ['top', 'Processes', 'always_ff']);
    assert.deepEqual(vm.innermostAt(tree, (u) => u === 'file:///a.sv', 10, 0).map((x) => x.id), ['top']);
  });
  test.it('ignores other files and inherited members', () => {
    assert.deepEqual(vm.innermostAt(tree, (u) => u === 'file:///b.sv', 4, 0), []);
    assert.deepEqual(vm.innermostAt(tree, (u) => u === 'file:///a.sv', 2, 3).map((x) => x.id), ['top']);
    assert.deepEqual(vm.innermostAt(tree, (u) => u === 'file:///a.sv', 30, 0).map((x) => x.id), ['c']);
  });
  test.it('finds an extern method in its body, outside its class', () => {
    const f = n('f', 'function', at(24, 2, 24, 30), [], { body: at(50, 0, 53, 11) });
    const pkg = [n('p', 'package', at(21, 0, 60, 10), [n('Classes', 'group', undefined, [n('c', 'class', at(22, 0, 40, 8), [n('Methods', 'group', undefined, [f])])])])];
    assert.deepEqual(vm.innermostAt(pkg, (u) => u === 'file:///a.sv', 51, 4).map((x) => x.id), ['p', 'Classes', 'c', 'Methods', 'f']);
    assert.deepEqual(vm.innermostAt(pkg, (u) => u === 'file:///a.sv', 45, 0).map((x) => x.id), ['p']);
  });
});

test.describe('register model texts', () => {
  test.it('say where a node is, its width and access', () => {
    assert.equal(vm.registerDescription({ name: 'ctrl', kind: 'register', address: '0x10', bits: '32 bits', rights: 'RW' }), '0x10 · 32 bits · RW');
    assert.equal(vm.registerDescription({ name: 'r', kind: 'register', offset: 'base + 4', bits: '32 bits' }), 'offset base + 4 · 32 bits');
    assert.equal(vm.registerDescription({ name: 'sub', kind: 'block', class: 'sub_block', address: '0x1000' }), '0x1000 · sub_block');
    assert.equal(vm.registerDescription({ name: 'blk', kind: 'block', class: 'blk' }), undefined);
    assert.equal(vm.registerDescription({ name: 'bus', kind: 'map', address: '0x0', bits: '4 bytes' }), '0x0 · 4 bytes');
    assert.equal(vm.registerDescription({ name: 'en', kind: 'field', range: '[5:4]', access: 'RW', reset: '0x1' }), '[5:4] · RW · reset 0x1');
    assert.equal(vm.registerDescription({ name: 'en', kind: 'field', range: '[0]', access: 'RO', reset: null }), '[0] · RO');
  });
  test.it('mark nodes with notes and list the notes in the tooltip', () => {
    const n = { name: 'r[0]', kind: 'register', class: 'ctrl_reg', address: '0x30', bits: '32 bits', notes: ['added under a condition'] };
    assert.equal(vm.registerDescription(n), '0x30 · 32 bits ⚠');
    assert.equal(vm.registerDescription({ name: 'x', kind: 'register', notes: ['offset not constant'] }), '⚠');
    assert.equal(
      vm.registerTooltip(n, ['top', 'bus', 'r[0]']),
      ['**r[0]** register : `ctrl_reg`', 'top.bus.r[0]', 'Address: `0x30`', 'Size: `32 bits`', 'Added under a condition.'].join('\n\n'),
    );
    assert.match(vm.registerTooltip({ name: 'bus', kind: 'map', bits: '4 bytes' }, ['top', 'bus']), /Bus width: `4 bytes`/);
    assert.match(vm.registerTooltip({ name: 'en', kind: 'field', volatile: true, access: 'RC' }, ['top', 'bus', 'r', 'en']), /Access: `RC`\n\nVolatile\./);
  });
});

test.describe('assertions and coverage texts', () => {
  const hit = (o) => ({ kind: 'assert', group: 'checks', roles: ['checked'], detail: '', line: 9, children: [], ...o });
  test.it('name results without a label by what and where they are', () => {
    assert.equal(vm.checkLabel(hit({ label: 'a_req' })), 'a_req');
    assert.equal(vm.checkLabel(hit({ form: 'concurrent' })), 'assert property (line 10)');
    assert.equal(vm.checkLabel(hit({ form: 'immediate' })), 'assert (line 10)');
    assert.equal(vm.checkLabel(hit({ kind: 'cover sequence', form: 'concurrent' })), 'cover sequence (line 10)');
  });
  test.it('describe the form and the roles that are not the checked value', () => {
    assert.equal(vm.checkDescription(hit({ form: 'concurrent' })), undefined);
    assert.equal(vm.checkDescription(hit({ form: 'deferred', roles: ['checked', 'clock'] })), 'deferred · clock');
    assert.equal(vm.checkDescription(hit({ kind: 'port', group: 'connections', roles: ['connection'], notes: ['connected in an expression'] })), 'connected in an expression');
    assert.match(vm.checkTooltip(hit({ label: 'a', detail: 'a: assert property (x)', roles: ['checked', 'reset'] })), /```systemverilog\na: assert property \(x\)\n```\n\nAs: checked, reset/);
  });
  test.it('hide clock and reset uses, and connections left empty', () => {
    const tree = [
      hit({ label: 'a', roles: ['clock'] }),
      hit({ label: 'b', roles: ['clock', 'checked'] }),
      hit({ kind: 'port', group: 'connections', roles: ['connection'], children: [hit({ label: 'c', roles: ['reset'] })] }),
      hit({ kind: 'port', group: 'connections', roles: ['connection'], label: 'p', children: [hit({ label: 'd' })] }),
      hit({ kind: 'property', group: 'helpers', label: 'unused', children: [] }),
      hit({ kind: 'property', group: 'helpers', label: 'clocked', roles: ['clock'], children: [hit({ label: 'e' })] }),
    ];
    assert.deepEqual(vm.valuesOnly(tree).map((h) => h.label), ['b', 'p', 'unused'], 'a helper using it as a clock goes with what uses the helper');
  });
  test.it('say why nothing is shown or what limited the search', () => {
    assert.equal(vm.checksMessage('req', 0, [], []), "No assertion or coverage uses 'req' in this build configuration.");
    assert.equal(vm.checksMessage('clk', 0, [], [], 2), "'clk' is used only as a clock or reset (2 uses hidden: show all roles to see them).");
    assert.equal(vm.checksMessage('req', 3, ['candidates'], []), 'Search incomplete: too many names to look at.');
    assert.equal(vm.checksMessage('req', 3, [], ['not followed: bind to an instance path (dut.u)']), 'Not followed: bind to an instance path (dut.u).');
    assert.equal(vm.checksMessage('req', 3, [], []), undefined);
  });
});

test.describe('drivers and loads', () => {
  const site = (o) => ({ kind: 'blocking', group: 'writes', label: 'blocking', detail: 'q = d;', line: 4, children: [], ...o });
  test.it('labels a site by its line and a connection by its port', () => {
    assert.equal(vm.accessLabel(site()), 'q = d;');
    assert.equal(vm.accessLabel(site({ group: 'connections', kind: 'port', label: 'a of u_c (input)', detail: 'child u_c (.a(s));' })), 'a of u_c (input)');
    assert.equal(vm.accessDescription(site({ process: 'always_ff', context: 'u_c' })), 'blocking · in always_ff · u_c');
    assert.equal(vm.accessDescription(site({ group: 'connections', label: 'a of u_e (input)', notes: ['connected in an expression: not followed'] })), 'connected in an expression: not followed');
    assert.ok(vm.accessTooltip(site({ context: 'u_c.u_l' })).includes('Through instances: u_c.u_l'));
  });
  test.it('says when nothing was found or the search stopped', () => {
    assert.equal(vm.accessesMessage('q', 'writes', 0, [], []), "Nothing writes 'q' in this build configuration.");
    assert.equal(vm.accessesMessage('q', 'reads', 0, [], []), "Nothing reads 'q' in this build configuration.");
    assert.equal(vm.accessesMessage('q', 'reads', 2, ['depth'], ['`bind` instances in tb are not followed']), 'Search incomplete: connections followed only 3 levels deep. `bind` instances in tb are not followed.');
    assert.equal(vm.accessesMessage('q', 'reads', 2, [], []), undefined);
  });
});

test.describe('design hierarchy parameters', () => {
  const node = (o) => ({ name: 'u_fifo', definition: 'fifo', kind: 'module', truncated: false, ...o });
  test.it('describes the parameters given, the range and whether it exists', () => {
    const params = [
      { name: 'W', value: '16', state: 'evaluated', origin: 'override', kind: 'parameter' },
      { name: 'DEPTH', value: '8', state: 'evaluated', origin: 'default', kind: 'parameter' },
      { name: 'N', state: 'unresolved', why: 'a defparam with a path this version does not follow may set it', origin: 'defparam', kind: 'parameter' },
    ];
    assert.equal(vm.hierDescription(node({ params })), 'fifo · W=16 N=?');
    assert.equal(vm.hierDescription(node({ array: '[3:0] · 4 instances' })), 'fifo · [3:0]');
    assert.equal(vm.hierDescription(node({ presence: 'undetermined: condition not evaluated: `P` is not resolved' })), 'fifo · undetermined');
    assert.equal(vm.hierDescription({ name: 'g[0]', definition: '', kind: 'generate', truncated: false }), undefined);
    const tip = vm.hierTooltip(node({ params, presence: 'not elaborated: condition false' }), 'top.u_fifo');
    assert.ok(tip.includes('*Not elaborated: condition false.*'), tip);
    assert.ok(tip.includes('- `W` = `16` · given'), tip);
    assert.ok(tip.includes('- `N` = ? (unresolved: a defparam with a path this version does not follow may set it) · defparam'), tip);
  });
});

test.describe('choosing the compile context', () => {
  const item = (n, root, chosen = false, invocation = 0) => ({ root: `file:///p/${root}`, invocation_key: 'v1-k', nth: 0, before: null, ordinal: n, invocation, mode: 'default', root_role: 'top', includer: { uri: `file:///p/${root}`, line: 1 }, chosen });
  const name = (u) => u.replace('file:///p/', '');
  const list = { total: 450, matched: 450, offset: 0, state: 'chosen', chosen: item(300, 'r300.sv', true), default: item(1, 'r1.sv'), unavailable: null, items: [item(1, 'r1.sv'), item(2, 'r2.sv', false, 4)] };
  test.it('shows the context in use in the status bar, nothing for one', () => {
    assert.deepEqual(vm.contextStatus(list, name), { text: '$(layers) 300 of 450 *', tooltip: 'Compiled as r300.sv, invocation 1: chosen, of 450 contexts. Click to choose another.' });
    assert.equal(vm.contextStatus({ ...list, total: 1 }, name), undefined);
    const gone = vm.contextStatus({ ...list, state: 'unavailable', chosen: null, unavailable: ['file:///p/b.sv', 'v1-x', 0] }, name);
    assert.match(gone.text, /^\$\(warning\) 1 of 450$/);
    assert.match(gone.tooltip, /not compiled in this build/);
  });
  test.it('lists the default, the choice beyond the page, the page and a row for the next one', () => {
    const rows = vm.contextPickRows(list, name, false);
    assert.deepEqual(
      rows.map((r) => r.label),
      ['Default (first in build order)', '$(check) 300. r300.sv', '1. r1.sv', '2. r2.sv', '$(ellipsis) 448 more: show the next 2'],
    );
    assert.equal(rows[0].choice, null);
    assert.deepEqual(rows[1].choice, { root: 'file:///p/r300.sv', invocation_key: 'v1-k', nth: 0, before: null });
    assert.equal(rows[3].description, 'invocation 5 · top file');
    assert.equal(rows[2].detail, 'r1.sv — included at r1.sv:2');
    assert.equal(rows[4].more, 2);
    assert.equal(rows[4].choice, undefined);
    // A search answer, or a further page: the page only.
    assert.deepEqual(vm.contextPickRows({ ...list, matched: 2 }, name, true).map((r) => r.label), ['1. r1.sv', '2. r2.sv']);
  });
  test.it('forgets every kept choice of the file, whatever its spelling', () => {
    // Two client uris of one engine file (a drive and its WSL path), and the engine's spelling
    // of another case, written as VS Code and as the engine write them.
    const serverOf = (f) => (f.startsWith('file://wsl.localhost/') ? 'file:///mnt/c/p/h.svh' : f.replace('file:///c%3A/', 'file:///mnt/c/'));
    const norm = (u) => u.replace('%3A', ':').toLowerCase();
    const kept = [{ file: 'file:///c%3A/p/h.svh' }, { file: 'file://wsl.localhost/u/mnt/c/p/h.svh' }, { file: 'file:///c%3A/p/H.SVH' }, { file: 'file:///c%3A/p/other.svh' }];
    const left = vm.othersKept(kept, 'file://wsl.localhost/u/mnt/c/p/h.svh', ['file:///mnt/c/p/H.SVH'], serverOf, norm);
    assert.deepEqual(left.map((k) => k.file), ['file:///c%3A/p/other.svh']);
  });
  test.it('marks the chosen context in the report', () => {
    const c = {
      uri: 'file:///p/h.svh', known: true, auto: false, build_file: null, skip_matches: [], detail: null, contexts_total: 2, choice: 'chosen',
      contexts: [
        { invocation: 0, mode: 'default', root_role: 'top', root: 'file:///p/a.sv', order: null, is_root: false, ordinal: 1, chosen: false, chain: [], chain_cut: false },
        { invocation: 1, mode: 'default', root_role: 'top', root: 'file:///p/b.sv', order: null, is_root: false, ordinal: 2, chosen: true, chain: [], chain_cut: false },
      ],
    };
    const md = vm.compileContextMarkdown(c, (u) => `<${u}>`, name);
    assert.match(md, /use the context chosen with the command \*DVICode: Compile This File As\.\.\.\*/);
    assert.match(md, /2\. From \[b\.sv\]\(<file:\/\/\/p\/b\.sv>\) .* — \*\*chosen\*\*/);
    const gone = vm.compileContextMarkdown({ ...c, choice: 'unavailable', unavailable: ['file:///p/b.sv', 'v1-x', 0] }, (u) => `<${u}>`, name);
    assert.match(gone, /The context chosen for this file \(from \[b\.sv\]\(<file:\/\/\/p\/b\.sv>\)\) is not compiled in this build/);
    const none = vm.compileContextMarkdown({ ...c, contexts: [], contexts_total: 0, choice: 'unavailable', unavailable: ['file:///p/b.sv', 'v1-x', 0] }, (u) => `<${u}>`, name);
    assert.match(none, /A compile context was chosen for this file \(from \[b\.sv\].*it is kept[\s\S]*\*\*Not compiled\.\*\*/);
  });
});
