# constraint-name

A constraint is not named `<what>_cstr`.

- Rulesets: hexk
- Default level: warning
- Style guide: [LNG-13](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

A constraint whose name does not end in `_cstr`, unless a base class declares a constraint of
that name (an override keeps it).

## Why

One suffix makes constraints easy to find and to turn off by name (style guide LNG-13).

## Example

```systemverilog bad
class packet;
  rand int len;
  constraint len_c { len < 64; }
endclass
```

```systemverilog good
class packet;
  rand int len;
  constraint len_cstr { len < 64; }
endclass
```

## Waive

One finding: `// LNG-13 waived: <reason>` on the line before the statement or declaration. The rule: `"constraint-name": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
