# extern-method

A class method declared `extern`.

- Rulesets: hexk
- Default level: warning
- Style guide: [CLS-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A function or task of a class declared `extern` (its body defined outside the class).

## Why

Two copies of every signature cost more than they give, and editors outline classes anyway
(style guide CLS-2).

## Example

```systemverilog bad
package p;
  class c;
    extern function void f();
  endclass
  function void c::f(); endfunction
endpackage
```

```systemverilog good
package p;
  class c;
    function void f(); endfunction
  endclass
endpackage
```

## Waive

One finding: `// CLS-2 waived: <reason>` on the line before the statement or declaration. The rule: `"extern-method": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
