# ascii-only

A line holds a character outside ASCII.

- Rulesets: hexk
- Default level: warning
- Style guide: [FMT-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A line of a SystemVerilog file that holds a character outside ASCII (one finding per line):
typographic quotes, non-breaking spaces, letters with accents.

## Why

Tools differ in how they read other encodings, and these characters are often invisible in
review (style guide FMT-2). Documentation may use UTF-8; sources may not.

## Example

```systemverilog bad
module m;
  // résumé of the block
endmodule
```

```systemverilog good
module m;
  // summary of the block
endmodule
```

## Waive

One finding: `// FMT-2 waived: <reason>` on the line before the statement or declaration. The rule: `"ascii-only": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
