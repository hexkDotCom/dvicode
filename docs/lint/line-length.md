# line-length

A line is longer than the limit.

- Rulesets: hexk
- Default level: info
- Style guide: [FMT-3](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A line longer than `max` characters (120 by default), tabs counted to the next multiple of
`tab-width` (4). Set them in `.dvicode/lint.json`:
`"line-length": { "level": "info", "max": 100 }`.

## Why

Long lines are hard to read side by side in reviews and diffs (style guide FMT-3, a SHOULD:
the default level is info).

## Example

```systemverilog bad
module m;
  localparam int A_VERY_LONG_PARAMETER_NAME_THAT_GOES_ON = 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13 + 14 + 15 + 16 + 17;
endmodule
```

```systemverilog good
module m;
  localparam int SUM = 1 + 2 + 3;
endmodule
```

## Waive

One finding: `// FMT-3 waived: <reason>` on the line before the statement or declaration. The rule: `"line-length": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
