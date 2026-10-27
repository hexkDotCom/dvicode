# delay-without-unit

A delay literal without a time unit.

- Rulesets: hexk
- Default level: warning
- Style guide: [FIL-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A delay whose value is a number without a time unit: `#10`, `#(10)`, `a = #5 b;`,
`assign #2 a = b;`. `#0`, `#1step`, cycle delays (`##1`) and delays computed from names
(`#(period * 2)`) are not reported.

## Why

A bare `#10` means whatever the scope's time unit is (style guide FIL-7).

## Example

```systemverilog bad
module m;
  timeunit 1ns;
  timeprecision 1ps;
  bit a;
  initial #10 a = 1;
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

One finding: `// FIL-7 waived: <reason>` on the line before the statement or declaration. The rule: `"delay-without-unit": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
