# program-block

A `program` block.

- Rulesets: hexk
- Default level: warning
- Style guide: [IFC-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A `program` declaration.

## Why

Program blocks change event scheduling, which hides races instead of fixing them; clocks and
resets are generated in modules (style guide IFC-2).

## Example

```systemverilog bad
program test_prog;
endprogram
```

```systemverilog good
module test_top;
endmodule
```

## Waive

One finding: `// IFC-2 waived: <reason>` on the line before the statement or declaration. The rule: `"program-block": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
