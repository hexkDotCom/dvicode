# factory-incompatible-constructor

The factory cannot call the constructor of a registered class.

- Rulesets: recommended
- Default level: warning
- Style guide: [CLS-8](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)
- Subchecks: `component`, `object`

## What it finds

A class registered with the UVM factory whose constructor the factory cannot call:

- `component`: a component's `new` must accept `(name, parent)`;
- `object`: an object's `new` must accept both `()` and `(name)`, so its first argument needs a
  default value.

Arguments after those need default values. A class without a constructor has the implicit
`new()`, which fits neither components nor objects. Abstract registrations are not checked
(the factory never constructs them). With `UVM_OBJECT_DO_NOT_NEED_CONSTRUCTOR` defined, an
object needs only `new()`, and the implicit one fits.

## Why

The factory's generated code calls the constructor with exactly these arguments; a constructor
that needs others makes the code fail to compile where the class is registered, an error
reported far from the class.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_item extends uvm_sequence_item;
    `uvm_object_utils(demo_item)
    function new(string name); super.new(name); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_item extends uvm_sequence_item;
    `uvm_object_utils(demo_item)
    function new(string name = "demo_item"); super.new(name); endfunction
  endclass
endpackage
```

## Waive

One finding: `// CLS-8 waived: <reason>` on the line before the statement or declaration. The rule: `"factory-incompatible-constructor": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
