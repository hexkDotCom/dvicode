# uvm-override-signature

A UVM phase method is overridden with another signature.

- Rulesets: recommended
- Default level: warning
- Subchecks: `kind`, `arguments`, `return`

## What it finds

A method of a component named like a UVM phase (`build_phase`, `run_phase`, ...) that does not
match UVM's own declaration: a task where UVM declares a function (`kind`), other arguments
(`arguments`), or another return type (`return`).

## Why

UVM calls its phase methods through their virtual declarations. A method that does not match
does not override them: the compiler refuses it late, or UVM never calls it.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    task build_phase(uvm_phase phase); endtask
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); super.build_phase(phase); endfunction
  endclass
endpackage
```

## Waive

One finding: `// uvm-override-signature waived: <reason>` on the line before the statement or declaration. The rule: `"uvm-override-signature": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
