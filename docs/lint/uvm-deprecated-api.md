# uvm-deprecated-api

Deprecated UVM API is used.

- Rulesets: recommended
- Default level: warning
- Style guide: [FAC-11](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

Uses of the UVM API that UVM 1.2 deprecated and IEEE 1800.2 removed: `set_config_*`/
`get_config_*`, `global_stop_request`, `stop_request`, `set_global_timeout`, `uvm_test_done`,
`starting_phase`, `print_config_settings`, and the sequence-library and sequencer macros
(`` `uvm_sequence_utils ``, `` `uvm_sequencer_utils ``, ...). Only names that resolve to the UVM
library's own declarations are reported.

## Why

Code that uses them does not compile with a current UVM library, nor with UVM 1.2 built with
`+define+UVM_NO_DEPRECATED`.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      set_config_int("*", "count", 4);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

## Waive

One finding: `// FAC-11 waived: <reason>` on the line before the statement or declaration. The rule: `"uvm-deprecated-api": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
