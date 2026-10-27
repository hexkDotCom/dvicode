# coverage-weight-goal

`option.weight` or `option.goal` is set.

- Rulesets: hexk
- Default level: warning
- Style guide: [COV-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`option.weight` or `option.goal` set in a covergroup, coverpoint or cross, or through a
covergroup handle, to anything but its default: `weight` 1 and `goal` 100, in any literal form
(`32'd1`), are not reported. `option.weight = 0` on a coverpoint that a cross of its covergroup
names (one that exists to build the cross) is the guide's exception and is not reported.

## Why

A goal under 100 hides holes, and weights turn the total into a negotiation (style guide
COV-7).

## Example

```systemverilog bad
module m;
  bit [1:0] mode;
  covergroup cg;
    option.goal = 90;
    coverpoint mode;
  endgroup
endmodule
```

```systemverilog good
module m;
  bit [1:0] mode;
  covergroup cg;
    coverpoint mode;
  endgroup
endmodule
```

## Waive

One finding: `// COV-7 waived: <reason>` on the line before the statement or declaration. The rule: `"coverage-weight-goal": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
