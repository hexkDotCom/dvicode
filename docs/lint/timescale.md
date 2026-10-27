# timescale

A `` `timescale `` directive.

- Rulesets: hexk
- Default level: warning
- Style guide: [FIL-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A `` `timescale `` directive.

## Why

`` `timescale `` applies to every file compiled after it, so a file's time unit depends on the
compile order. Declare `timeunit` and `timeprecision` in each package, module and interface
that needs them (style guide FIL-7).

## Example

```systemverilog bad
`timescale 1ns/1ps
module m;
endmodule
```

```systemverilog good
module m;
  timeunit 1ns;
  timeprecision 1ps;
endmodule
```

## Waive

One finding: `// FIL-7 waived: <reason>` on the line before the statement or declaration. The rule: `"timescale": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
