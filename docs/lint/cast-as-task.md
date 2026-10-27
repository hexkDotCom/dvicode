# cast-as-task

`$cast` called as a task.

- Rulesets: hexk
- Default level: warning
- Style guide: [LNG-20](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A statement that is only `$cast(dst, src);`: the cast called as a task.

## Why

A failed task-form `$cast` is a run-time error that simulators may continue past with the
destination unchanged. Call it as a function and report a failure (style guide LNG-20).

## Example

```systemverilog bad
package p;
  class a; endclass
  class b extends a; endclass
  function void f(a x);
    b y;
    $cast(y, x);
  endfunction
endpackage
```

```systemverilog good
package p;
  class a; endclass
  class b extends a; endclass
  function void f(a x);
    b y;
    if (!$cast(y, x)) $fatal(1, "not a b");
  endfunction
endpackage
```

## Fix

The quick fix wraps the call in `if (!$cast(...))` with `` `uvm_fatal `` (or `$fatal` where
UVM's macros are not defined).

## Waive

One finding: `// LNG-20 waived: <reason>` on the line before the statement or declaration. The rule: `"cast-as-task": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
