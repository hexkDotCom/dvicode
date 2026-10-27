# randomize-in-assert

randomize() is called inside an assertion.

- Rulesets: recommended
- Default level: warning
- Style guide: [LNG-10](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A `randomize()` call inside the condition of an immediate or deferred `assert` or `assume`.

## Why

Turning assertions off (`$assertoff`, a simulator option, a coverage-only run) skips the whole
assertion, and the randomization with it: the test then runs on values that were never
randomized.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    assert (randomize());
  endfunction
endclass
```

```systemverilog good
class packet;
  rand int len;
  function void make();
    if (!randomize()) $fatal(1, "packet randomization failed");
  endfunction
endclass
```

## Waive

One finding: `// LNG-10 waived: <reason>` on the line before the statement or declaration. The rule: `"randomize-in-assert": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
