# suppression-not-waiver

A `dvicode-lint:` suppression.

- Rulesets: hexk
- Default level: info

## What it finds

Every `// dvicode-lint: disable=...` and `disable-file=...` suppression (an information).

## Why

Suppressions are a tool's switch, not the style guide's waiver record: the guide records each
exception as `// <ID> waived: <reason>` on the code it covers (§0). This rule makes the
suppressions visible so that they can be turned into waivers.

## Example

```systemverilog bad
module m;
  // dvicode-lint: disable=trailing-whitespace generated code
endmodule
```

```systemverilog good
module m;
endmodule
```

## Waive

One finding: `// suppression-not-waiver waived: <reason>` on the line before the statement or declaration. The rule: `"suppression-not-waiver": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
