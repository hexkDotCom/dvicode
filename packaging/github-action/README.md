# DVICode Lint action

Lints SystemVerilog and UVM code with [DVICode](https://github.com/hexkDotCom/dvicode): findings
appear as annotations on the pull request and, with SARIF upload, in code scanning.

```yaml
name: Lint
on: [push, pull_request]
permissions:
  contents: read
  security-events: write   # for the SARIF upload
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: hexkDotCom/dvicode-lint-action@v1
        with:
          path: .
          args: --rulesets recommended
```

| Input | Default | |
|---|---|---|
| `version` | the action's pinned release | the DVICode release to download (checked against its `SHA256SUMS`) |
| `path` | `.` | the project folder (its build file, or auto-configuration) |
| `args` | | more `dvicode lint` arguments: `--rulesets hexk`, `--build FILE`, `--baseline FILE`, `--changed-since origin/main` (fetch the history first) |
| `fail-on` | `warning` | the lowest level that fails the step (`error`, `warning`, `info`, `never`) |
| `sarif` | `true` | upload SARIF to code scanning (the job needs `security-events: write`) |

Rules come from the project's `.dvicode/lint.json`. See the
[lint guide](https://github.com/hexkDotCom/dvicode/blob/main/docs/lint.md) and the
[rule reference](https://github.com/hexkDotCom/dvicode/blob/main/docs/lint/README.md).

The SARIF is uploaded even when the lint step fails, so the findings are in code scanning
either way. Apache-2.0.
