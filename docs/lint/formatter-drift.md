# formatter-drift

Lines differ from what the project's formatter makes.

- Rulesets: hexk
- Default level: warning
- Style guide: [FMT-13](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

When `.dvicode/lint.json` says `"formatter": "dvicode"`: the lines that DVICode's formatter,
with the project's `.dvicode/format.json`, would change (one finding per run of lines).

## Why

A project that names its formatter keeps its code as the formatter makes it (style guide
FMT-13). Put the formatter's options in `.dvicode/format.json` so that every editor and the
CLI use the same ones.

## Example

```systemverilog bad
module m;
wire a;
endmodule
//@ file: .dvicode/lint.json
{ "formatter": "dvicode" }
```

```systemverilog good
module m;
  wire a;
endmodule
//@ file: .dvicode/lint.json
{ "formatter": "dvicode" }
```

## Waive

One finding: `// FMT-13 waived: <reason>` on the line before the statement or declaration. The rule: `"formatter-drift": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
