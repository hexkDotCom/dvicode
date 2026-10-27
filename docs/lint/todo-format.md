# todo-format

A TODO is not written `TODO:<owner>:<P1|P2|P3>: <text>`.

- Rulesets: hexk
- Default level: warning
- Style guide: [DOC-6](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A comment with a `TODO` that is not written `TODO:<owner>:<P1|P2|P3>: <text>`.

## Why

One form makes TODOs greppable by owner and priority, and P1s can be checked before sign-off
(style guide DOC-6).

## Example

```systemverilog bad
module m;
  // TODO handle the reset
endmodule
```

```systemverilog good
module m;
  // TODO:rg:P2: handle the reset
endmodule
```

## Waive

A waiver cannot cover where this rule reports. Turn it off for some lines with `// dvicode-lint: disable=todo-format <reason>` and `// dvicode-lint: enable=todo-format`, or for a file with `// dvicode-lint: disable-file=todo-format <reason>`. The rule: `"todo-format": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
