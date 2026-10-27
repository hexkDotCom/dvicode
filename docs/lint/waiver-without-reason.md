# waiver-without-reason

A waiver or suppression gives no reason.

- Rulesets: recommended
- Default level: warning

## What it finds

A waiver (`// <ID> waived: <reason>`) or a `dvicode-lint: disable=` suppression with no reason,
or with a placeholder (`<reason>`, `TODO`).

## Why

The style guide (§0) makes the reason part of every waiver: it is what reviewers check and what
later readers need.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    // LNG-10 waived:
    void'(randomize());
  endfunction
endclass
```

```systemverilog good
class packet;
  rand int len;
  function void make();
    // LNG-10 waived: values do not matter in this smoke test
    void'(randomize());
  endfunction
endclass
```

## Waive

One finding: `// waiver-without-reason waived: <reason>` on the line before the statement or declaration. The rule: `"waiver-without-reason": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
