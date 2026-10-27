# compilation-unit-import

An `import` at compilation-unit scope.

- Rulesets: hexk
- Default level: warning
- Style guide: [FIL-4](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

An `import` at compilation-unit scope: outside any module, interface or package.

## Why

It applies to every file compiled after it, so a file's names depend on the compile order
(style guide FIL-4).

## Example

```systemverilog bad
package util_pkg;
endpackage
import util_pkg::*;
module m;
endmodule
```

```systemverilog good
package util_pkg;
endpackage
module m;
  import util_pkg::*;
endmodule
```

## Waive

One finding: `// FIL-4 waived: <reason>` on the line before the statement or declaration. The rule: `"compilation-unit-import": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
