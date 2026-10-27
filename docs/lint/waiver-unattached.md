# waiver-unattached

A waiver is followed by no statement or declaration it could cover.

- Rulesets: hexk
- Default level: info

## What it finds

A waiver followed by nothing it could cover: right before `end`, `else`, `endcase`, an
`` `ifdef `` or the end of the file.

## Why

A waiver covers the statement or declaration right after it; one in the wrong place covers
nothing.

## Example

```systemverilog bad
module m;
  initial begin
    // LNG-10 waived: misplaced
  end
endmodule
```

```systemverilog good
module m;
  initial begin
  end
endmodule
```

## Waive

One finding: `// waiver-unattached waived: <reason>` on the line before the statement or declaration. The rule: `"waiver-unattached": "off"` in `.dvicode/lint.json`, for all files or under `overrides` for some.
