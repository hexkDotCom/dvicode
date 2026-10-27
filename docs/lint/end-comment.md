# end-comment

A comment follows `end` on its line.

- Rulesets: hexk
- Default level: warning
- Style guide: [FMT-7](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A comment on the same line right after `end`, `join`, `join_any`, `join_none` or `endcase`.

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

One finding: `// FMT-7 waived: <reason>` on the line before the statement or declaration. The rule: `"end-comment": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
