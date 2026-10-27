# trailing-whitespace

A line ends in spaces or tabs.

- Rulesets: hexk
- Default level: info
- Style guide: [FMT-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

Spaces or tabs at the end of a line (outside strings).

## Why

Invisible differences that show up in diffs and reviews (style guide FMT-2).

## Example

```systemverilog bad
module m;@@
endmodule
```

```systemverilog good
module m;
endmodule
```

(`@@` stands for a trailing space in this page.)

## Waive

A waiver cannot cover where this rule reports. Turn it off for some lines with `// dvicode-lint: disable=trailing-whitespace <reason>` and `// dvicode-lint: enable=trailing-whitespace`, or for a file with `// dvicode-lint: disable-file=trailing-whitespace <reason>`. The rule: `"trailing-whitespace": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
