# dvicode: SystemVerilog/UVM lint and checks for the command line

`dvicode` is the engine of [DVICode](https://github.com/hexkDotCom/dvicode), the SystemVerilog
and UVM extension for VS Code, Cursor and VSCodium. It compiles a whole project the way the
editor does (build files, compile contexts, packages, the UVM library) and checks it in CI.

```bash
dvicode lint .                            # the recommended rules; exit 1 on findings
dvicode lint . --rulesets hexk            # the HEXK SV/UVM Style Guide's rules too
dvicode lint . --format sarif --output lint.sarif
dvicode lint --list-rules
dvicode check .                           # compile problems only
dvicode --stdio                           # the language server, for any LSP editor
```

- **`recommended`** finds what the compiler does not, and only where it can prove it: a
  `randomize()` whose result is thrown away or called inside an `assert`, a UVM registration
  macro that registers another class, a constructor the factory cannot call, a phase method
  that does not override UVM's, deprecated UVM API.
- **`hexk`** adds the checkable rules of the
  [HEXK SystemVerilog/UVM Style Guide](https://github.com/hexkDotCom/sv-uvm-style-guide).

Configure it in `.dvicode/lint.json`. Waive a single finding in the code with
`// <rule or guide ID> waived: <reason>`. Adopt it on existing code with a baseline
(`--write-baseline`, `--baseline`) or with `--changed-since main`. Output formats: text, JSON,
SARIF 2.1.0, JUnit XML, GitHub annotations.

- Guide: https://github.com/hexkDotCom/dvicode/blob/main/docs/lint.md
- Rules: https://github.com/hexkDotCom/dvicode/blob/main/docs/lint/README.md

Free and open source (Apache-2.0). The rule IDs and adapted rule texts of the style guide are
CC BY 4.0, Copyright 2026 HEXK (see NOTICE).
