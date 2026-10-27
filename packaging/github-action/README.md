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
| `args` | | more `dvicode lint` arguments: `--rulesets hexk`, `--build FILE`, `--baseline FILE`, `--changed-since origin/main` (fetch the history first). Split into words as a shell would, keeping quoted parts together; variables and wildcards are not expanded |
| `fail-on` | `warning` | the lowest level that fails the step (`error`, `warning`, `info`, `hint`, `never`) |
| `sarif` | `true` | upload SARIF to code scanning (the job needs `security-events: write`) |

Rules come from the project's `.dvicode/lint.json`. See the
[lint guide](https://github.com/hexkDotCom/dvicode/blob/main/docs/lint.md) and the
[rule reference](https://github.com/hexkDotCom/dvicode/blob/main/docs/lint/README.md).

The SARIF is uploaded even when lint fails, so the findings are in code scanning either way;
the action's last step then fails the job. Annotations are limited to ten per level (what
GitHub shows of a step), and long messages are cut: the SARIF has every finding. On pull requests from forks the token is read-only and cannot upload it: use
`sarif: ${{ !github.event.pull_request.head.repo.fork }}`. Apache-2.0.
