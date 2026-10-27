# file-extension

A compiled file or an included file has the wrong extension.

- Rulesets: hexk
- Default level: warning
- Style guide: [FIL-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A compiled file whose name does not end in `.sv`; an included file that ends neither in `.sv`
nor in `.svh`; a `.svh` file that holds code (classes, functions, design elements) rather than
macros, types and parameters.

## Why

One extension for code makes files easy to find and tools easy to configure (style guide
FIL-2).

## Example

```systemverilog bad
package p;
  `include "bus_item.svh"
endpackage
//@ file: bus_item.svh
class bus_item;
endclass
```

```systemverilog good
package p;
  `include "bus_defs.svh"
endpackage
//@ file: bus_defs.svh
`define BUS_WIDTH 32
```

## Waive

One finding: `// FIL-2 waived: <reason>` on the line before the statement or declaration. The rule: `"file-extension": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
