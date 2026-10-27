# waiver-unknown-id

A waiver or suppression names a rule that does not exist.

- Rulesets: recommended
- Default level: warning

## What it finds

A waiver or suppression that names no lint rule and no rule of the style guide.

## Why

A misspelled ID waives nothing: the finding it was meant for stays, or a later one is not
noticed.

## Example

```systemverilog bad
class packet;
  // LNG-100 waived: wrong number
  rand int len;
endclass
```

```systemverilog good
class packet;
  rand int len;
endclass
```

## Waive

Its findings are about waivers themselves and cannot be waived or suppressed in the code: correct the comment. The rule: `"waiver-unknown-id": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
