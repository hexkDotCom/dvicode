//! The documentation page of each rule: what it finds, why, examples, how to fix and waive.
//!
//! Each page holds at least one ```` ```systemverilog bad ```` and one ```` ```systemverilog good ````
//! example, complete files that the tests lint (the bad ones must give the rule's finding, the
//! good ones none). The pages are written to `docs/lint/` by `dvicode lint --docs`.

/// The page body of rule `id` (Markdown, after the generated title and summary).
pub fn page(id: &str) -> Option<&'static str> {
    Some(match id {
        "randomize-result-discarded" => RANDOMIZE_RESULT_DISCARDED,
        "randomize-in-assert" => RANDOMIZE_IN_ASSERT,
        "uvm-registration-mismatch" => REGISTRATION_MISMATCH,
        "factory-incompatible-constructor" => INCOMPATIBLE_CONSTRUCTOR,
        "uvm-override-signature" => OVERRIDE_SIGNATURE,
        "uvm-deprecated-api" => DEPRECATED_API,
        "trailing-whitespace" => TRAILING_WHITESPACE,
        "program-block" => PROGRAM_BLOCK,
        "timescale" => TIMESCALE_PAGE,
        "extern-method" => EXTERN_METHOD,
        "illegal-bins" => ILLEGAL_BINS_PAGE,
        "extends-uvm-transaction" => EXTENDS_UVM_TRANSACTION,
        "field-macros-in-component" => FIELD_MACROS_PAGE,
        "uvm-do-macros" => UVM_DO_PAGE,
        "p-sequencer" => P_SEQUENCER_PAGE,
        "cast-as-task" => CAST_AS_TASK_PAGE,
        "randomize-void-cast" => RANDOMIZE_VOID_CAST,
        "display-in-uvm-class" => DISPLAY_PAGE,
        "suppression-not-waiver" => SUPPRESSION_PAGE,
        "ascii-only" => ASCII_ONLY,
        "final-newline" => FINAL_NEWLINE,
        "line-length" => LINE_LENGTH,
        "end-label" => END_LABEL,
        "end-comment" => END_COMMENT,
        "todo-format" => TODO_FORMAT,
        "waiver-without-reason" => WAIVER_WITHOUT_REASON,
        "waiver-unknown-id" => WAIVER_UNKNOWN_ID,
        "waiver-unattached" => WAIVER_UNATTACHED,
        "suppression-unmatched" => SUPPRESSION_UNMATCHED,
        _ => return None,
    })
}

const RANDOMIZE_RESULT_DISCARDED: &str = r#"## What it finds

A call to the built-in `randomize()` used as a statement on its own: `item.randomize();`,
`this.randomize() with {...};`, `std::randomize(x);`. The result, 0 when the constraints cannot
be solved, is thrown away.

## Why

A failed randomization leaves the variables as they were. The test goes on with stale or
default values and can pass while it checks nothing it meant to check.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    randomize() with { len inside {[1:64]}; };
  endfunction
endclass
```

```systemverilog good
class packet;
  rand int len;
  function void make();
    if (!randomize() with { len inside {[1:64]}; }) $fatal(1, "packet randomization failed");
  endfunction
endclass
```

With UVM, report the failure with `` `uvm_fatal ``, as the style guide asks (LNG-10). A
deliberate discard can be written `void'(x.randomize());` (allowed by `recommended`, reported
by `hexk`).

## Fix

The quick fix wraps the statement: `if (!x.randomize()) `uvm_fatal("RAND", "x.randomize() failed")`
(with `$fatal` where UVM's macros are not defined).

## Not reported

A `randomize` call inside a macro argument or produced by a macro, a receiver whose type is not
known, and a method that the project itself declares under that name: these are counted as
unknown (see `--stats`).
"#;

const RANDOMIZE_IN_ASSERT: &str = r#"## What it finds

A `randomize()` call inside the condition of an immediate or deferred `assert` or `assume`.

## Why

Turning assertions off (`$assertoff`, a simulator option, a coverage-only run) skips the whole
assertion, and the randomization with it: the test then runs on values that were never
randomized.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    assert (randomize());
  endfunction
endclass
```

```systemverilog good
class packet;
  rand int len;
  function void make();
    if (!randomize()) $fatal(1, "packet randomization failed");
  endfunction
endclass
```
"#;

const REGISTRATION_MISMATCH: &str = r#"## What it finds

A UVM registration macro (`` `uvm_object_utils ``, `` `uvm_component_utils `` and their
`_param`, `_abstract` and `_begin` forms) that does not fit the class it is written in:

- `other-class`: it names another class;
- `family`: the object macro on a component, or the component macro on an object;
- `param`: a name-registering macro on a parameterized class;
- `abstract`: a concrete macro on a `virtual class`, where the UVM library has abstract
  registration (IEEE 1800.2).

## Why

The factory creates what the macro registers. Another class's name makes the factory create
that class; the wrong family or form fails to compile only where the factory is first used, or
registers a parameterized class under one name for every specialization.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    `uvm_object_utils(demo_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    `uvm_component_utils(demo_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

## Fix

`family` has a quick fix (the macro of the other family, same form); `param` offers the
`_param` macro as a suggestion (it changes how the class is found by name).
"#;

const INCOMPATIBLE_CONSTRUCTOR: &str = r#"## What it finds

A class registered with the UVM factory whose constructor the factory cannot call:

- `component`: a component's `new` must accept `(name, parent)`;
- `object`: an object's `new` must accept both `()` and `(name)`, so its first argument needs a
  default value.

Arguments after those need default values. A class without a constructor has the implicit
`new()`, which fits neither components nor objects.

## Why

The factory's generated code calls the constructor with exactly these arguments; a constructor
that needs others makes the code fail to compile where the class is registered, an error
reported far from the class.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_item extends uvm_sequence_item;
    `uvm_object_utils(demo_item)
    function new(string name); super.new(name); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_item extends uvm_sequence_item;
    `uvm_object_utils(demo_item)
    function new(string name = "demo_item"); super.new(name); endfunction
  endclass
endpackage
```
"#;

const OVERRIDE_SIGNATURE: &str = r#"## What it finds

A method of a component named like a UVM phase (`build_phase`, `run_phase`, ...) that does not
match UVM's own declaration: a task where UVM declares a function (`kind`), other arguments
(`arguments`), or another return type (`return`).

## Why

UVM calls its phase methods through their virtual declarations. A method that does not match
does not override them: the compiler refuses it late, or UVM never calls it.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    task build_phase(uvm_phase phase); endtask
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase); super.build_phase(phase); endfunction
  endclass
endpackage
```
"#;

const DEPRECATED_API: &str = r#"## What it finds

Uses of the UVM API that UVM 1.2 deprecated and IEEE 1800.2 removed: `set_config_*`/
`get_config_*`, `global_stop_request`, `stop_request`, `set_global_timeout`, `uvm_test_done`,
`starting_phase`, `print_config_settings`, and the sequence-library and sequencer macros
(`` `uvm_sequence_utils ``, `` `uvm_sequencer_utils ``, ...). Only names that resolve to the UVM
library's own declarations are reported.

## Why

Code that uses them does not compile with a current UVM library, nor with UVM 1.2 built with
`+define+UVM_NO_DEPRECATED`.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      set_config_int("*", "count", 4);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package demo_pkg;
  import uvm_pkg::*;
  class demo_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```
"#;

const TRAILING_WHITESPACE: &str = r#"## What it finds

Spaces or tabs at the end of a line (outside strings).

## Why

Invisible differences that show up in diffs and reviews (style guide FMT-2).

## Example

```systemverilog bad
module m;@@
endmodule
```

```systemverilog good
module m;
endmodule
```

(`@@` stands for a trailing space in this page.)
"#;

const ASCII_ONLY: &str = r#"## What it finds

A line of a SystemVerilog file that holds a character outside ASCII (one finding per line):
typographic quotes, non-breaking spaces, letters with accents.

## Why

Tools differ in how they read other encodings, and these characters are often invisible in
review (style guide FMT-2). Documentation may use UTF-8; sources may not.

## Example

```systemverilog bad
module m;
  // résumé of the block
endmodule
```

```systemverilog good
module m;
  // summary of the block
endmodule
```
"#;

const FINAL_NEWLINE: &str = r#"## What it finds

A file that does not end with exactly one newline: none at all, or blank lines at the end.

## Why

Without one, the last line joins whatever follows the file (another file in a list); blank
lines at the end are noise in diffs (style guide FMT-2).

## Example

```systemverilog bad
module m;
endmodule
@@EOF
```

```systemverilog good
module m;
endmodule
```
"#;

const LINE_LENGTH: &str = r#"## What it finds

A line longer than `max` characters (120 by default), tabs counted to the next multiple of
`tab-width` (4). Set them in `.dvicode/lint.json`:
`"line-length": { "level": "info", "max": 100 }`.

## Why

Long lines are hard to read side by side in reviews and diffs (style guide FMT-3, a SHOULD:
the default level is info).

## Example

```systemverilog bad
module m;
  localparam int A_VERY_LONG_PARAMETER_NAME_THAT_GOES_ON = 1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13 + 14 + 15 + 16 + 17;
endmodule
```

```systemverilog good
module m;
  localparam int SUM = 1 + 2 + 3;
endmodule
```
"#;

const END_LABEL: &str = r#"## What it finds

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
"#;

const END_COMMENT: &str = r#"## What it finds

A comment on the same line right after `end`, `join`, `join_any`, `join_none` or `endcase`.

## Why

End comments (`end // while (...)`) go stale when the code above changes, and nothing checks
them (style guide FMT-7). Turn off the editor setting that inserts them.

## Example

```systemverilog bad
module m;
  initial begin
  end // initial
endmodule
```

```systemverilog good
module m;
  initial begin
  end
endmodule
```
"#;

const TODO_FORMAT: &str = r#"## What it finds

A comment with a `TODO` that is not written `TODO:<owner>:<P1|P2|P3>: <text>`.

## Why

One form makes TODOs greppable by owner and priority, and P1s can be checked before sign-off
(style guide DOC-6).

## Example

```systemverilog bad
module m;
  // TODO handle the reset
endmodule
```

```systemverilog good
module m;
  // TODO:rg:P2: handle the reset
endmodule
```
"#;

const PROGRAM_BLOCK: &str = r#"## What it finds

A `program` declaration.

## Why

Program blocks change event scheduling, which hides races instead of fixing them; clocks and
resets are generated in modules (style guide IFC-2).

## Example

```systemverilog bad
program test_prog;
endprogram
```

```systemverilog good
module test_top;
endmodule
```
"#;

const TIMESCALE_PAGE: &str = r#"## What it finds

A `` `timescale `` directive.

## Why

`` `timescale `` applies to every file compiled after it, so a file's time unit depends on the
compile order. Declare `timeunit` and `timeprecision` in each package, module and interface
that needs them (style guide FIL-7).

## Example

```systemverilog bad
`timescale 1ns/1ps
module m;
endmodule
```

```systemverilog good
module m;
  timeunit 1ns;
  timeprecision 1ps;
endmodule
```
"#;

const EXTERN_METHOD: &str = r#"## What it finds

A function or task of a class declared `extern` (its body defined outside the class).

## Why

Two copies of every signature cost more than they give, and editors outline classes anyway
(style guide CLS-2).

## Example

```systemverilog bad
package p;
  class c;
    extern function void f();
  endclass
  function void c::f(); endfunction
endpackage
```

```systemverilog good
package p;
  class c;
    function void f(); endfunction
  endclass
endpackage
```
"#;

const ILLEGAL_BINS_PAGE: &str = r#"## What it finds

`illegal_bins` in a covergroup.

## Why

Illegal values are errors: assertions or checkers report them. Coverage only samples, and
`illegal_bins` errors depend on the simulator and on sampling. Exclude the values with
`ignore_bins` (style guide COV-6).

## Example

```systemverilog bad
module m;
  bit [1:0] mode;
  covergroup cg;
    coverpoint mode { illegal_bins bad = {3}; }
  endgroup
endmodule
```

```systemverilog good
module m;
  bit [1:0] mode;
  covergroup cg;
    coverpoint mode { ignore_bins unused = {3}; }
  endgroup
endmodule
```
"#;

const EXTENDS_UVM_TRANSACTION: &str = r#"## What it finds

A class whose base is UVM's `uvm_transaction`.

## Why

Sequence items extend `uvm_sequence_item`, which sequences and sequencers work with (style guide
TRN-1).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_transaction;
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```
"#;

const FIELD_MACROS_PAGE: &str = r#"## What it finds

A UVM field macro (`` `uvm_field_int `` and the others) in a component.

## Why

Field macros in a component make it read the configuration database implicitly
(`apply_config_settings`); configuration is explicit in the style guide (FAC-4).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    int count;
    `uvm_component_utils_begin(bus_env)
      `uvm_field_int(count, UVM_DEFAULT)
    `uvm_component_utils_end
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    int count;
    `uvm_component_utils(bus_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
endpackage
```
"#;

const UVM_DO_PAGE: &str = r#"## What it finds

Calls of UVM's `` `uvm_do ``, `` `uvm_create ``, `` `uvm_send `` and `` `uvm_rand_send `` macro
families. Project macros that wrap them are not followed.

## Why

They hide `create`, `start_item`, `randomize` (and whether it succeeded) and `finish_item`;
written out, each step is visible and the randomization is checked (style guide FAC-7).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_item;
    uvm_sequence_item req;
    task body();
      `uvm_do(req)
    endtask
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_item;
    task body();
    endtask
  endclass
endpackage
```
"#;

const P_SEQUENCER_PAGE: &str = r#"## What it finds

`` `uvm_declare_p_sequencer ``.

## Why

It ties a sequence to one sequencer type, so the sequence cannot run elsewhere (style guide
FAC-8; SEQ-8 lists alternatives).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_item;
    `uvm_declare_p_sequencer(uvm_component)
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_item;
  endclass
endpackage
```
"#;

const CAST_AS_TASK_PAGE: &str = r#"## What it finds

A statement that is only `$cast(dst, src);`: the cast called as a task.

## Why

A failed task-form `$cast` is a run-time error that simulators may continue past with the
destination unchanged. Call it as a function and report a failure (style guide LNG-20).

## Example

```systemverilog bad
package p;
  class a; endclass
  class b extends a; endclass
  function void f(a x);
    b y;
    $cast(y, x);
  endfunction
endpackage
```

```systemverilog good
package p;
  class a; endclass
  class b extends a; endclass
  function void f(a x);
    b y;
    if (!$cast(y, x)) $fatal(1, "not a b");
  endfunction
endpackage
```

## Fix

The quick fix wraps the call in `if (!$cast(...))` with `` `uvm_fatal `` (or `$fatal` where
UVM's macros are not defined).
"#;

const RANDOMIZE_VOID_CAST: &str = r#"## What it finds

`void'(x.randomize());`: a randomization whose result is discarded on purpose.

## Why

The style guide asks for every `randomize()` result to be checked and reported with
`` `uvm_fatal `` (LNG-10). `recommended` accepts `void'` as an explicit discard; `hexk` does not.
A unit test of a randomizer that expects a failure may waive it.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    void'(randomize());
  endfunction
endclass
```

```systemverilog good
class packet;
  rand int len;
  function void make();
    if (!randomize()) $fatal(1, "packet randomization failed");
  endfunction
endclass
```
"#;

const DISPLAY_PAGE: &str = r#"## What it finds

`$display`, `$write`, `$monitor`, `$strobe` (and their `b`, `h`, `o` forms) in a method of a
class derived from a UVM class.

## Why

UVM classes report with the UVM report macros, which the test can filter, count, redirect and
stop on; `$display` bypasses all of that (style guide MSG-1).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      $display("done");
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_env extends uvm_env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      `uvm_info("ENV", "done", UVM_LOW)
    endfunction
  endclass
endpackage
```
"#;

const SUPPRESSION_PAGE: &str = r#"## What it finds

Every `// dvicode-lint: disable=...` and `disable-file=...` suppression (an information).

## Why

Suppressions are a tool's switch, not the style guide's waiver record: the guide records each
exception as `// <ID> waived: <reason>` on the code it covers (§0). This rule makes the
suppressions visible so that they can be turned into waivers.

## Example

```systemverilog bad
module m;
  // dvicode-lint: disable=trailing-whitespace generated code
endmodule
```

```systemverilog good
module m;
endmodule
```
"#;

const WAIVER_WITHOUT_REASON: &str = r#"## What it finds

A waiver (`// <ID> waived: <reason>`) or a `dvicode-lint: disable=` suppression with no reason,
or with a placeholder (`<reason>`, `TODO`).

## Why

The style guide (§0) makes the reason part of every waiver: it is what reviewers check and what
later readers need.

## Example

```systemverilog bad
class packet;
  rand int len;
  function void make();
    // LNG-10 waived:
    void'(randomize());
  endfunction
endclass
```

```systemverilog good
class packet;
  rand int len;
  function void make();
    // LNG-10 waived: values do not matter in this smoke test
    void'(randomize());
  endfunction
endclass
```
"#;

const WAIVER_UNKNOWN_ID: &str = r#"## What it finds

A waiver or suppression that names no lint rule and no rule of the style guide.

## Why

A misspelled ID waives nothing: the finding it was meant for stays, or a later one is not
noticed.

## Example

```systemverilog bad
class packet;
  // LNG-100 waived: wrong number
  rand int len;
endclass
```

```systemverilog good
class packet;
  rand int len;
endclass
```
"#;

const WAIVER_UNATTACHED: &str = r#"## What it finds

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
"#;

const SUPPRESSION_UNMATCHED: &str = r#"## What it finds

A `// dvicode-lint: enable=<rule>` with no `disable=` of that rule open before it.

## Why

It usually means the `disable` was removed or misspelled, and the region it meant to end was
never suppressed.

## Example

```systemverilog bad
module m;
  // dvicode-lint: enable=trailing-whitespace
endmodule
```

```systemverilog good
module m;
endmodule
```
"#;

/// The examples of a page: (is it the bad one, the code).
pub fn examples(page: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut lines = page.lines();
    while let Some(l) = lines.next() {
        let bad = match l.trim() {
            "```systemverilog bad" => true,
            "```systemverilog good" => false,
            _ => continue,
        };
        let mut code = String::new();
        for c in lines.by_ref() {
            if c.trim() == "```" {
                break;
            }
            // `@@EOF`: the file ends without a newline here.
            if c.trim() == "@@EOF" {
                code.pop();
                continue;
            }
            code.push_str(&c.replace("@@", " "));
            code.push('\n');
        }
        out.push((bad, code));
    }
    out
}
