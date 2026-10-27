# randomize-void-cast

The result of randomize() is discarded with void'(...).

- Rulesets: hexk
- Default level: warning
- Style guide: [LNG-10](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`void'(x.randomize());`: a randomization whose result is discarded on purpose.

## Why

The style guide asks for every `randomize()` result to be checked and reported with
`` `uvm_fatal `` (LNG-10). `recommended` accepts `void'` as an explicit discard; `hexk` does not.
A unit test of a randomizer that expects a failure may waive it.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    void'(randomize());
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

One finding: `// LNG-10 waived: <reason>` on the line before the statement or declaration. The rule: `"randomize-void-cast": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
