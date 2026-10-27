# final-newline

The file does not end with exactly one newline.

- Rulesets: hexk
- Default level: warning
- Style guide: [FMT-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A file that does not end with exactly one newline: none at all, or blank lines at the end.

## Why

Without one, the last line joins whatever follows the file (another file in a list); blank
lines at the end are noise in diffs (style guide FMT-2).

## Example

```systemverilog bad
module m;
endmodule
```

(The file ends without a newline after its last line.)

```systemverilog good
module m;
endmodule
```

## Waive

A waiver cannot cover where this rule reports. Turn it off for some lines with `// dvicode-lint: disable=final-newline <reason>` and `// dvicode-lint: enable=final-newline`, or for a file with `// dvicode-lint: disable-file=final-newline <reason>`. The rule: `"final-newline": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
