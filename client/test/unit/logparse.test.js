// Unit tests of src/logparse.ts: the links found in simulator log lines (synthesized lines in
// the formats the tools print; no vendor log is kept in the repository).

const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');

const lp = require(path.join(__dirname, '..', '..', 'out', 'unit', 'logparse.js'));

/** The links of a line, as `kind text -> path:line`. */
function show(line) {
  return lp.lineLinks(line).map((l) => `${l.kind} ${line.slice(l.start, l.end)} -> ${l.path}${l.kind === 'file' ? `:${l.line}` : ''}`);
}

test.describe('lineLinks', () => {
  test.it('reads UVM report lines: the file and the component', () => {
    assert.deepEqual(show('UVM_ERROR ../tb/sb.sv(142) @ 1500: uvm_test_top.env.sb [SB] mismatch'), [
      'file ../tb/sb.sv(142) -> ../tb/sb.sv:142',
      'component uvm_test_top.env.sb -> uvm_test_top.env.sb',
    ]);
    assert.deepEqual(show('UVM_INFO /nfs/p/tb/drv.sv(88) @ 15.000 ns: uvm_test_top.env.agents[0].drv [DRV] sent'), [
      'file /nfs/p/tb/drv.sv(88) -> /nfs/p/tb/drv.sv:88',
      'component uvm_test_top.env.agents[0].drv -> uvm_test_top.env.agents[0].drv',
    ]);
  });
  test.it('links a sequence report to its sequencer, and no reporter that is not a component', () => {
    assert.deepEqual(show('UVM_INFO seq.sv(10) @ 0: uvm_test_top.env.sqr@@seq [SEQ] start'), ['file seq.sv(10) -> seq.sv:10', 'component uvm_test_top.env.sqr -> uvm_test_top.env.sqr']);
    assert.deepEqual(show('UVM_INFO @ 0: reporter [RNTST] Running test my_test...'), []);
  });
  test.it('reads quoted locations (VCS) with any extension', () => {
    assert.deepEqual(show('Error: "/nfs/p/tb/top.sv", 45: top.u_chk: at time 100 ns'), ['file "/nfs/p/tb/top.sv", 45 -> /nfs/p/tb/top.sv:45']);
    assert.deepEqual(show('  "rtl/a.vp", 7: token is \'endmodule\''), ['file "rtl/a.vp", 7 -> rtl/a.vp:7']);
  });
  test.it('reads file(line), file:line and Xcelium file,line|col', () => {
    assert.deepEqual(show('** Error: /p/x.sv(10): (vlog-2730) Undefined variable'), ['file /p/x.sv(10) -> /p/x.sv:10']);
    assert.deepEqual(show('see C:\\work\\x.svh:12:3 for details'), ['file C:\\work\\x.svh:12 -> C:\\work\\x.svh:12']);
    assert.deepEqual(show('xmvlog: *E,EXPSMC (./tb.sv,12|8): expecting a semicolon'), ['file ./tb.sv,12|8 -> ./tb.sv:12']);
  });
  test.it('ignores other text', () => {
    assert.deepEqual(show('Parsing design file x.c'), []);
    assert.deepEqual(show('time 10:20 done'), []);
  });
});

test.describe('textLinks', () => {
  const links = (text) => lp.textLinks(text, 100).map((l) => `${text.slice(l.start, l.end)} -> ${l.kind === 'file' ? `${l.path}:${l.line}` : l.path}`);
  test.it('reads a VCS message location on the line after its header', () => {
    const text = 'Warning-[TMBIN] Too many bits in Based Number\n/nfs/p/rtl/dma.sv, 129\n  The specified width is 2 bits.\n';
    assert.deepEqual(links(text), ['/nfs/p/rtl/dma.sv, 129 -> /nfs/p/rtl/dma.sv:129']);
    assert.deepEqual(links('Lint-[TFIPC-L] Too few instance port connections\r\n\r\nD:/p/m.sv, 374\r\n'), ['D:/p/m.sv, 374 -> D:/p/m.sv:374']);
  });
  test.it('does not take prose after a header for a location', () => {
    assert.deepEqual(links('Warning-[X] Title\n  The value is 2, 3\n'), []);
  });
  test.it('reads an elaboration task location over three lines, or one', () => {
    const text = 'Warning-[WEST] $warning elaboration system task\n  msg: no match\n  location: file \n  /nfs/p/ram.sv\n  line 98\n';
    assert.deepEqual(links(text), ['/nfs/p/ram.sv -> /nfs/p/ram.sv:98']);
    assert.deepEqual(links('  location: file /nfs/p/ram.sv line 7\n'), ['/nfs/p/ram.sv -> /nfs/p/ram.sv:7']);
  });
  test.it('gives offsets in the text and stops at the limit', () => {
    const text = 'x\nUVM_ERROR a.sv(1) @ 0: uvm_test_top [A] m\nUVM_ERROR b.sv(2) @ 0: uvm_test_top [B] m\n';
    assert.deepEqual(links(text), ['a.sv(1) -> a.sv:1', 'uvm_test_top -> uvm_test_top', 'b.sv(2) -> b.sv:2', 'uvm_test_top -> uvm_test_top']);
    assert.equal(lp.textLinks(text, 3).length, 3);
  });
});

test.describe('review cases', () => {
  test.it('reads a very long line quickly (the patterns do not scan back and forth)', () => {
    const t0 = Date.now();
    assert.deepEqual(show('a'.repeat(60000) + ':1'), []);
    assert.deepEqual(lp.textLinks('x'.repeat(200000) + '.sv(3)\n', 10).length, 0, 'past the part of a line read');
    assert.ok(Date.now() - t0 < 200, `${Date.now() - t0} ms`);
  });
  test.it('links a UVM report without a file in a log', () => {
    const text = 'UVM_INFO @ 0: uvm_test_top.env [ID] hello\n';
    assert.deepEqual(lp.textLinks(text, 10).map((l) => text.slice(l.start, l.end)), ['uvm_test_top.env']);
  });
  test.it('reads coloured lines, with offsets in the line as written', () => {
    const line = 'UVM_ERROR \u001b[31m/nfs/p/a.sv\u001b[0m(12) @ 0: uvm_test_top.sb [SB] m';
    assert.deepEqual(lp.lineLinks(line).map((l) => [l.kind, l.path]), [['file', '/nfs/p/a.sv'], ['component', 'uvm_test_top.sb']]);
    const text = '\u001b[1mWarning-[TMBIN]\u001b[0m Too many bits\n/nfs/p/b.sv, 4\n';
    assert.deepEqual(lp.textLinks(text, 10).map((l) => `${l.path}:${l.line}`), ['/nfs/p/b.sv:4']);
  });
  test.it('reads paths with spaces where the format ends them', () => {
    assert.deepEqual(show('UVM_INFO C:\\work dir\\x.sv(12) @ 0: uvm_test_top.env [ID] hello').map((s) => s.split(' -> ')[1]), ['C:\\work dir\\x.sv:12', 'uvm_test_top.env']);
    const text = 'Error-[SE] Syntax error\nC:\\work dir\\x.sv, 12\n';
    assert.deepEqual(lp.textLinks(text, 10).map((l) => `${l.path}:${l.line}`), ['C:\\work dir\\x.sv:12']);
  });
  test.it('reads a report with its verbosity', () => {
    assert.deepEqual(show('UVM_INFO(UVM_LOW) a.sv(1) @ 0: uvm_test_top.env [ID] hello'), ['file a.sv(1) -> a.sv:1', 'component uvm_test_top.env -> uvm_test_top.env']);
  });
  test.it('keeps a budget for errors and warnings when ordinary reports use theirs', () => {
    const info = 'UVM_INFO a.sv(1) @ 0: uvm_test_top.env [I] m\n'.repeat(50);
    const text = info + 'UVM_ERROR b.sv(2) @ 9: uvm_test_top.sb [E] bad\n';
    const paths = lp.textLinks(text, 10).map((l) => l.path);
    assert.ok(paths.includes('b.sv'), paths.join(' '));
  });
});

test.describe('LinkScanner', () => {
  test.it('reads a log a part at a time as it reads it at once, across the parts', () => {
    const text = 'Warning-[TMBIN] Too many bits\n/nfs/p/a.sv, 3\nUVM_ERROR b.sv(2) @ 0: uvm_test_top.env [B] m\n  location: file\n  /nfs/p/c.sv\n  line 9\n';
    const s = new lp.LinkScanner(text, 100);
    let steps = 0;
    while (!s.step(1)) steps++;
    assert.deepEqual(s.links.map((l) => `${l.path}:${l.line ?? ''}`), lp.textLinks(text, 100).map((l) => `${l.path}:${l.line ?? ''}`));
    assert.ok(steps >= 6, `${steps} steps`);
  });
});

test.describe('logTest', () => {
  test.it('reads the test UVM ran', () => {
    assert.equal(lp.logTest('UVM_INFO @ 0: reporter [RNTST] Running test demo_full_test...\n'), 'demo_full_test');
    assert.equal(lp.logTest('no test here'), undefined);
  });
});
