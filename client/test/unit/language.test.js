// The language configuration: valid JSON (VS Code reads it leniently, so a bad escape would
// silently break a pattern) and the indentation and word patterns on typical lines.

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const cfg = JSON.parse(fs.readFileSync(path.join(__dirname, '..', '..', 'language-configuration.json'), 'utf8'));

test.describe('Language configuration', () => {
  test.it('indents after headers and blocks, not after prototypes or one-line bodies', () => {
    const inc = new RegExp(cfg.indentationRules.increaseIndentPattern);
    for (const t of ['  if (a) begin', '  begin : named', 'class c extends b;', '  virtual function void build_phase(uvm_phase phase);', 'module top (input logic clk);', '  fork', '  case (s)']) {
      assert.ok(inc.test(t), t);
    }
    for (const t of ['  extern function void f();', '  pure virtual task t();', '  virtual interface bus_if vif;', '  function void f(); endfunction', '  if (a) begin x = 1; end', '  case (s) 0: x = 0; default: x = 1; endcase', '  int x;']) {
      assert.ok(!inc.test(t), t);
    }
    assert.ok(new RegExp(cfg.indentationRules.decreaseIndentPattern).test('  endfunction'));
  });
  test.it('takes escaped identifiers as words and finds region markers', () => {
    assert.equal('x = \\bus+idx ;'.match(new RegExp(cfg.wordPattern, 'g'))[1], '\\bus+idx');
    assert.ok(new RegExp(cfg.folding.markers.start).test('  // #region setup'));
    assert.ok(new RegExp(cfg.folding.markers.end).test('// endregion'));
  });
});
