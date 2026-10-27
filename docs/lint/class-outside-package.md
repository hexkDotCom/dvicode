# class-outside-package

A class declared outside a package.

- Rulesets: hexk
- Default level: warning
- Style guide: [FIL-3](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A class declared at compilation-unit scope, or in a module or interface. Files the
configuration marks as `examples` may declare classes in their module (a self-contained
example or unit test, as FIL-3 allows).

## Why

A class in a package has one definition that the whole testbench shares (style guide FIL-3).

## Example

```systemverilog bad
class packet;
endclass
```

```systemverilog good
package p;
  class packet;
  endclass
endpackage
```

## Waive

One finding: `// FIL-3 waived: <reason>` on the line before the statement or declaration. The rule: `"class-outside-package": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
