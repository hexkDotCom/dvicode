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

test.describe('placeTree', () => {
  const t = vm.placeTree(design, (n) => n.name);
  test.it('gives repeated sibling names their own keys and keeps the names', () => {
    const keys = vm.depthFirst(t.roots).map((p) => p.key.split('\u0000').join('/'));
    assert.deepEqual(keys, ['top', 'top/c0', 'top/c0/u_rx', 'top/c0/u_rx#2', 'top/c1', 'top/c1/u_rx', 'top/g', 'top/g/f']);
    assert.equal(t.byNames.get(vm.namesKey(['top', 'c0', 'u_rx'])).length, 2);
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
