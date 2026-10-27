# end-label

A class, function, task, module, interface, package, covergroup, property, sequence or clocking block ends without a label.

- Rulesets: hexk
- Default level: warning
- Style guide: [FMT-6](https://github.com/hexkDotCom/sv-uvm-style-guide/blob/main/sv_uvm_style_guide.md)

## What it finds

`endclass`, `endfunction`, `endtask`, `endmodule`, `endinterface`, `endpackage`, `endgroup`,
`endproperty`, `endsequence` and `endclocking` without `: <name>`.

## Why

The compiler checks an end label against the name it closes; it does not check a comment
(style guide FMT-6).

## Example

```systemverilog bad
module m;
endmodule
```

```systemverilog good
module m;
endmodule : m
```

## Waive

One finding: `// FMT-6 waived: <reason>` on the line before the statement or declaration. The rule: `"end-label": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
