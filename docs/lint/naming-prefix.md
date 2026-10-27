# naming-prefix

A name starts with `uvm_` or `m_`.

- Rulesets: hexk
- Default level: warning
- Style guide: [NAM-3](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A declaration or macro whose name starts with `uvm_` or `m_`, except overrides of library
methods.

## Why

`uvm_` belongs to the library and `m_` marks UVM's internals; project names with them hide
that distinction (style guide NAM-3).

## Example

```systemverilog bad
class m_driver;
endclass
```

```systemverilog good
class bus_driver;
endclass
```

## Waive

One finding: `// NAM-3 waived: <reason>` on the line before the statement or declaration. The rule: `"naming-prefix": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
