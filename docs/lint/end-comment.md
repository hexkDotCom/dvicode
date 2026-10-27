# end-comment

A comment follows `end` on its line.

- Rulesets: hexk
- Default level: warning
- Style guide: [FMT-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A comment on the same line right after `end` (or `end : label`). Comments after `join`,
`join_any`, `join_none` and `endcase` are not checked.

## Why

End comments (`end // while (...)`) go stale when the code above changes, and nothing checks
them (style guide FMT-7). Turn off the editor setting that inserts them.

## Example

```systemverilog bad
module m;
  initial begin
  end // initial
endmodule
```

```systemverilog good
module m;
  initial begin
  end
endmodule
```

## Waive

A waiver cannot cover where this rule reports. Turn it off for some lines with `// dvicode-lint: disable=end-comment <reason>` and `// dvicode-lint: enable=end-comment`, or for a file with `// dvicode-lint: disable-file=end-comment <reason>`. The rule: `"end-comment": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
