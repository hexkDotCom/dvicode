# timeunit-first

A scope with delays or time values does not declare timeunit and timeprecision first.

- Rulesets: hexk
- Default level: warning
- Style guide: [FIL-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A module, interface or package that holds a delay or a time literal of its own (not one of a
nested design element) and does not declare both `timeunit` and `timeprecision` as its first
items after its header (`timeunit 1ns / 1ps;` declares both).

## Why

Without them, the scope's time unit comes from compile order or tool defaults (style guide
FIL-7).

## Example

```systemverilog bad
module m;
  bit a;
  initial #10ns a = 1;
endmodule
```

```systemverilog good
module m;
  timeunit 1ns;
  timeprecision 1ps;
  bit a;
  initial #10ns a = 1;
endmodule
```

## Waive

One finding: `// FIL-7 waived: <reason>` on the line before the statement or declaration. The rule: `"timeunit-first": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
