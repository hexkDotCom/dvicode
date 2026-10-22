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
