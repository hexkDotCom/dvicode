# illegal-bins

`illegal_bins` in a covergroup.

- Rulesets: hexk
- Default level: warning
- Style guide: [COV-6](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`illegal_bins` in a covergroup.

## Why

Illegal values are errors: assertions or checkers report them. Coverage only samples, and
`illegal_bins` errors depend on the simulator and on sampling. Exclude the values with
`ignore_bins` (style guide COV-6).

## Example

```systemverilog bad
module m;
  bit [1:0] mode;
  covergroup cg;
    coverpoint mode { illegal_bins bad = {3}; }
  endgroup
endmodule
```

```systemverilog good
module m;
  bit [1:0] mode;
  covergroup cg;
    coverpoint mode { ignore_bins unused = {3}; }
  endgroup
endmodule
```

## Waive

One finding: `// COV-6 waived: <reason>` on the line before the statement or declaration. The rule: `"illegal-bins": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
