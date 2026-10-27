# naming-case

A name is not in the case the style guide asks for.

- Rulesets: hexk
- Default level: warning
- Style guide: [NAM-1](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md), [NAM-2](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)
- Subchecks: `lower`, `upper`

## What it finds

Names not in the case the guide asks for: `lower_snake_case` for classes, variables, functions,
tasks, instances, signals, ports, arguments, packages, interfaces, modules and types;
`UPPER_SNAKE_CASE` for parameters, localparams, type parameters, enum literals and constant
macros (object-like macros whose body is one number or string). Escaped names, names written
by macros, and methods (with their arguments) that override a library method or implement an
interface class keep their spelling.

## Why

One convention, the UVM library's, everywhere (style guide NAM-1, NAM-2).

## Example

```systemverilog bad
class busDriver;
endclass
```

```systemverilog good
class bus_driver;
endclass
```

## Waive

One finding: `// NAM-1 waived: <reason>` on the line before the statement or declaration. The rule: `"naming-case": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
