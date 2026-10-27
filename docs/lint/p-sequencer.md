# p-sequencer

`` `uvm_declare_p_sequencer ``.

- Rulesets: hexk
- Default level: warning
- Style guide: [FAC-8](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`` `uvm_declare_p_sequencer ``.

## Why

It ties a sequence to one sequencer type, so the sequence cannot run elsewhere (style guide
FAC-8; SEQ-8 lists alternatives).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_item;
    `uvm_declare_p_sequencer(uvm_component)
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_item;
  endclass
endpackage
```

## Waive

One finding: `// FAC-8 waived: <reason>` on the line before the statement or declaration. The rule: `"p-sequencer": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
