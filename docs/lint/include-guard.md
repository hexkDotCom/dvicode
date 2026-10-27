# include-guard

An included file has no include guard named after it.

- Rulesets: hexk
- Default level: warning
- Style guide: [FIL-6](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A file that the build includes and that is not wrapped, as a whole, in an include guard named
after it: `` `ifndef BUS_CFG_SVH `` / `` `define BUS_CFG_SVH `` first, the matching `` `endif ``
last. The name is the file name upper-cased, every run of other characters as `_`.

## Why

A guard makes a second `` `include `` harmless; a name taken from the file keeps two files from
sharing one guard (style guide FIL-6).

## Example

```systemverilog bad
package p;
  `include "bus_cfg.svh"
endpackage
//@ file: bus_cfg.svh
typedef int bus_word_t;
```

```systemverilog good
package p;
  `include "bus_cfg.svh"
endpackage
//@ file: bus_cfg.svh
`ifndef BUS_CFG_SVH
`define BUS_CFG_SVH
typedef int bus_word_t;
`endif
```

## Waive

One finding: `// FIL-6 waived: <reason>` on the line before the statement or declaration. The rule: `"include-guard": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
