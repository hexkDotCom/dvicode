# suppression-unmatched

`dvicode-lint: enable=` without a `disable=` before it.

- Rulesets: hexk
- Default level: info

## What it finds

A `// dvicode-lint: enable=<rule>` with no `disable=` of that rule open before it.

## Why

It usually means the `disable` was removed or misspelled, and the region it meant to end was
never suppressed.

## Example

```systemverilog bad
module m;
  // dvicode-lint: enable=trailing-whitespace
endmodule
```

```systemverilog good
module m;
endmodule
```

## Waive

One finding: `// suppression-unmatched waived: <reason>` on the line before the statement or declaration. The rule: `"suppression-unmatched": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
