# randomize-result-discarded

The result of randomize() is discarded.

- Rulesets: recommended
- Default level: warning
- Style guide: [LNG-10](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A call to the built-in `randomize()` used as a statement on its own: `item.randomize();`,
`this.randomize() with {...};`, `std::randomize(x);`. The result, 0 when the constraints cannot
be solved, is thrown away.

## Why

A failed randomization leaves the variables as they were. The test goes on with stale or
default values and can pass while it checks nothing it meant to check.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    randomize() with { len inside {[1:64]}; };
  endfunction
endclass
```

```systemverilog good
class packet;
  rand int len;
  function void make();
    if (!randomize() with { len inside {[1:64]}; }) $fatal(1, "packet randomization failed");
  endfunction
endclass
```

With UVM, report the failure with `` `uvm_fatal ``, as the style guide asks (LNG-10). A
deliberate discard can be written `void'(x.randomize());` (allowed by `recommended`, reported
by `hexk`).

## Fix

The quick fix wraps the statement: `if (!x.randomize()) `uvm_fatal("RAND", "x.randomize() failed")`
(with `$fatal` where UVM's macros are not defined).

## Not reported

A `randomize` call inside a macro argument or produced by a macro, a receiver whose type is not
known, and a method that the project itself declares under that name: these are counted as
unknown (see `--stats`).

## Waive

One finding: `// LNG-10 waived: <reason>` on the line before the statement or declaration. The rule: `"randomize-result-discarded": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
