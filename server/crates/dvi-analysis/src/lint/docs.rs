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
        "uvm-class-not-registered" => NOT_REGISTERED_PAGE,
        "missing-super-phase-call" => MISSING_SUPER_PAGE,
        "component-created-with-new" => CREATED_WITH_NEW_PAGE,
        "uvm-internal-member" => INTERNAL_MEMBER_PAGE,
        "direct-report-call" => DIRECT_REPORT_PAGE,
        "message-id-get-name" => MESSAGE_ID_PAGE,
        "objection-without-description" => OBJECTION_PAGE,
        "default-sequence" => DEFAULT_SEQUENCE_PAGE,
        "pre-do-override" => PRE_DO_PAGE,
        "virtual-on-override" => VIRTUAL_OVERRIDE_PAGE,
        "constraint-name" => CONSTRAINT_NAME_PAGE,
        "create-name-chars" => CREATE_CHARS_PAGE,
        "create-name-matches" => CREATE_MATCHES_PAGE,
        "compilation-unit-import" => CU_IMPORT_PAGE,
        "class-outside-package" => CLASS_OUTSIDE_PAGE,
        "include-guard" => INCLUDE_GUARD_PAGE,
        "delay-without-unit" => DELAY_UNIT_PAGE,
        "timeunit-first" => TIMEUNIT_FIRST_PAGE,
        "file-extension" => FILE_EXTENSION_PAGE,
        "naming-case" => NAMING_CASE_PAGE,
        "naming-prefix" => NAMING_PREFIX_PAGE,
        "coverage-weight-goal" => WEIGHT_GOAL_PAGE,
        "formatter-drift" => FORMATTER_DRIFT_PAGE,
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

A `randomize` call inside a macro argument and a receiver whose type is not known: these are
counted as unknown (see `--stats`). A method, task or variable that the project itself declares
under that name is not the built-in and is left alone.
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
`new()`, which fits neither components nor objects. Abstract registrations are not checked
(the factory never constructs them). With `UVM_OBJECT_DO_NOT_NEED_CONSTRUCTOR` defined, an
object needs only `new()`, and the implicit one fits.

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

A comment on the same line right after `end` (or `end : label`). Comments after `join`,
`join_any`, `join_none` and `endcase` are not checked.

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
  class bus_seq extends uvm_sequence #(uvm_sequence_item);
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
  class bus_seq extends uvm_sequence #(uvm_sequence_item);
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

`$display` (and its `b`, `h`, `o` forms) in any class; in a class derived from a UVM class,
also `$write`, `$monitor`, `$strobe` (and their forms) and `$info`, `$warning`, `$error`,
`$fatal`.

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

const NOT_REGISTERED_PAGE: &str = r#"## What it finds

A concrete (not `virtual`) class derived from `uvm_object` (components included, TLM ports
not) with no UVM registration macro and no `type_id` of its own. When a project macro in the
class body may register it, the class is counted as unknown instead.

## Why

Only registered classes can be created and overridden through the factory (FAC-1, FAC-2).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    `uvm_object_utils(bus_item)
    function new(string name = "bus_item"); super.new(name); endfunction
  endclass
endpackage
```
"#;

const MISSING_SUPER_PAGE: &str = r#"## What it finds

A function phase of a component (`build_phase`, `connect_phase`, `end_of_elaboration_phase`,
`start_of_simulation_phase`, `extract_phase`, `check_phase`, `report_phase`, `final_phase`)
whose first statement, after declarations, is not an unconditional `super.<phase>(phase)`.
Task phases are not checked (CMP-4 depends on what the parent's phase does), nor overrides of
a pure virtual method.

## Why

The parent's phase work (UVM's own, or a base class's) is skipped, or runs after the derived
class's (style guide CMP-4).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      env = my_env::type_id::create("env", this);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = my_env::type_id::create("env", this);
    endfunction
  endclass
endpackage
```

## Fix

When the method calls no `super.<phase>` at all, the quick fix inserts the call first.
"#;

const CREATED_WITH_NEW_PAGE: &str = r#"## What it finds

`x = new(...)` (or a declaration with `= new`) where `x` is a component: a class derived from
`uvm_component` other than UVM's analysis FIFOs. TLM ports, exports and imps are not
components.

## Why

A component created with `new` cannot be replaced by a factory override from a test (style
guide FAC-1). Sequences, sequence items and configuration objects are also created through the
factory; this rule checks components.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = new("env", this);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = my_env::type_id::create("env", this);
    endfunction
  endclass
endpackage
```
"#;

const INTERNAL_MEMBER_PAGE: &str = r#"## What it finds

A reference to a member of the UVM library whose name starts with `m_` (UVM's internals),
except the documented `m_sequencer`.

## Why

UVM's internals change between versions without notice (style guide FAC-11).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    `uvm_object_utils(bus_item)
    function new(string name = "bus_item"); super.new(name); endfunction
    function string label(); return m_name; endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_item extends uvm_sequence_item;
    `uvm_object_utils(bus_item)
    function new(string name = "bus_item"); super.new(name); endfunction
    function string label(); return get_name(); endfunction
  endclass
endpackage
```
"#;

const DIRECT_REPORT_PAGE: &str = r#"## What it finds

A call of UVM's `uvm_report_info`, `uvm_report_warning`, `uvm_report_error` or
`uvm_report_fatal`. The report macros' own calls are not seen (they are made by the macro
expansion); a project macro that calls them is not followed.

## Why

The report macros skip building the message when its verbosity filters it out; a direct call
always builds it (style guide FAC-9).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      super.report_phase(phase);
      uvm_report_info("TEST", "done");
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      super.report_phase(phase);
      `uvm_info("TEST", "done", UVM_LOW)
    endfunction
  endclass
endpackage
```
"#;

const MESSAGE_ID_PAGE: &str = r#"## What it finds

A UVM report macro (`` `uvm_info `` and the others, `_context` forms too) whose message ID is
`get_name()` (with `this.`, `super.` or another receiver).

## Why

Instance names depend on where a class is instantiated and are already printed in the message
header; IDs should be static and functional, or `get_type_name()` (style guide MSG-2).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      super.report_phase(phase);
      `uvm_info(get_name(), "done", UVM_LOW)
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void report_phase(uvm_phase phase);
      super.report_phase(phase);
      `uvm_info("TEST_DONE", "done", UVM_LOW)
    endfunction
  endclass
endpackage
```
"#;

const OBJECTION_PAGE: &str = r#"## What it finds

A call of UVM's `raise_objection` or `drop_objection` (on a phase or an objection) whose
description argument is missing or `""`, by position or by name (`.description(...)`).

## Why

The description is what `+UVM_OBJECTION_TRACE` shows when an objection keeps a phase open
(style guide OBJ-4).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    task run_phase(uvm_phase phase);
      phase.raise_objection(this);
      phase.drop_objection(this, "test done");
    endtask
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    task run_phase(uvm_phase phase);
      phase.raise_objection(this, "test running");
      phase.drop_objection(this, "test done");
    endtask
  endclass
endpackage
```
"#;

const DEFAULT_SEQUENCE_PAGE: &str = r#"## What it finds

`default_sequence` set through UVM's `uvm_config_db::set` (the field name, third) or
`uvm_resource_db::set` (the name, second) as a string literal. The deprecated `set_config_*`
calls and names built at run time are not followed.

## Why

A default sequence starts stimulus implicitly from configuration; tests start their sequences
explicitly (style guide SEQ-11).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      uvm_config_db#(int)::set(this, "env.sqr.main_phase", "default_sequence", 0);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      uvm_config_db#(int)::set(this, "env", "count", 4);
    endfunction
  endclass
endpackage
```
"#;

const PRE_DO_PAGE: &str = r#"## What it finds

An override of UVM's sequence hooks `pre_do`, `mid_do` or `post_do` (an information).

## Why

These hooks run inside UVM's item flow; the style guide allows overriding them only to change a
sequence that cannot be edited, together with a factory override (SEQ-14). Waive the finding
where that is the case.

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_base;
    `uvm_object_utils(bus_seq)
    function new(string name = "bus_seq"); super.new(name); endfunction
    task pre_do(bit is_item); endtask
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class bus_seq extends uvm_sequence_base;
    `uvm_object_utils(bus_seq)
    function new(string name = "bus_seq"); super.new(name); endfunction
  endclass
endpackage
```
"#;

const VIRTUAL_OVERRIDE_PAGE: &str = r#"## What it finds

`virtual` written on a method that overrides a virtual method of the UVM library
(`build_phase`, `do_copy`, ...), directly or through project classes.

## Why

The override is virtual anyway; the style guide keeps the keyword for new methods that
extensions may override, so that it says something (CLS-4).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    virtual function void build_phase(uvm_phase phase);
      super.build_phase(phase);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
    endfunction
  endclass
endpackage
```

## Fix

The quick fix removes `virtual`.
"#;

const CONSTRAINT_NAME_PAGE: &str = r#"## What it finds

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
"#;

const CREATE_CHARS_PAGE: &str = r#"## What it finds

The name given to UVM's factory `create` as a string literal when it is empty, holds a
character outside `[a-zA-Z0-9_]`, or holds `__`.

## Why

Instance names are parts of hierarchical paths; other characters break config-db glob matching
and tools that read logs (style guide NAM-7).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = my_env::type_id::create("my env", this);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = my_env::type_id::create("env", this);
    endfunction
  endclass
endpackage
```
"#;

const CREATE_MATCHES_PAGE: &str = r#"## What it finds

`x = T::type_id::create("y", ...)` where the literal name is not the name of the variable it
is assigned to. Array elements and names built at run time are not checked.

## Why

Log paths then map directly to code (style guide NAM-8).

## Example

```systemverilog bad
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = my_env::type_id::create("environment", this);
    endfunction
  endclass
endpackage
```

```systemverilog good
`include "uvm_macros.svh"
package p;
  import uvm_pkg::*;
  class my_env extends uvm_env;
    `uvm_component_utils(my_env)
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
  endclass
  class my_test extends uvm_env;
    `uvm_component_utils(my_test)
    my_env env;
    function new(string name, uvm_component parent); super.new(name, parent); endfunction
    function void build_phase(uvm_phase phase);
      super.build_phase(phase);
      env = my_env::type_id::create("env", this);
    endfunction
  endclass
endpackage
```
"#;

const CU_IMPORT_PAGE: &str = r#"## What it finds

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
"#;

const CLASS_OUTSIDE_PAGE: &str = r#"## What it finds

A class declared at compilation-unit scope, or in a module or interface. Files the
configuration marks as `examples` may declare classes in their module (a self-contained
example or unit test, as FIL-3 allows).

## Why

A class in a package has one definition that the whole testbench shares (style guide FIL-3).

## Example

```systemverilog bad
class packet;
endclass
```

```systemverilog good
package p;
  class packet;
  endclass
endpackage
```
"#;

const INCLUDE_GUARD_PAGE: &str = r#"## What it finds

A file that the build includes and that is not wrapped, as a whole, in an include guard named
after it: `` `ifndef BUS_CFG_SVH `` / `` `define BUS_CFG_SVH `` first, the matching `` `endif ``
last. The name is the file name upper-cased, every run of other characters as `_`.

## Why

A guard makes a second `` `include `` harmless; a name taken from the file keeps two files from
sharing one guard (style guide FIL-6).

## Example

```systemverilog bad
package p;
  `include "bus_cfg.svh"
endpackage
//@ file: bus_cfg.svh
typedef int bus_word_t;
```

```systemverilog good
package p;
  `include "bus_cfg.svh"
endpackage
//@ file: bus_cfg.svh
`ifndef BUS_CFG_SVH
`define BUS_CFG_SVH
typedef int bus_word_t;
`endif
```
"#;

const DELAY_UNIT_PAGE: &str = r#"## What it finds

A delay whose value is a number without a time unit: `#10`, `#(10)`, `a = #5 b;`,
`assign #2 a = b;`. `#0`, `#1step`, cycle delays (`##1`) and delays computed from names
(`#(period * 2)`) are not reported.

## Why

A bare `#10` means whatever the scope's time unit is (style guide FIL-7).

## Example

```systemverilog bad
module m;
  timeunit 1ns;
  timeprecision 1ps;
  bit a;
  initial #10 a = 1;
endmodule
```

```systemverilog good
module m;
  timeunit 1ns;
  timeprecision 1ps;
  bit a;
  initial #10ns a = 1;
endmodule
```
"#;

const TIMEUNIT_FIRST_PAGE: &str = r#"## What it finds

A module, interface or package that holds a delay or a time literal of its own (not one of a
nested design element) and does not declare both `timeunit` and `timeprecision` as its first
items after its header (`timeunit 1ns / 1ps;` declares both).

## Why

Without them, the scope's time unit comes from compile order or tool defaults (style guide
FIL-7).

## Example

```systemverilog bad
module m;
  bit a;
  initial #10ns a = 1;
endmodule
```

```systemverilog good
module m;
  timeunit 1ns;
  timeprecision 1ps;
  bit a;
  initial #10ns a = 1;
endmodule
```
"#;

const FILE_EXTENSION_PAGE: &str = r#"## What it finds

A compiled file whose name does not end in `.sv`; an included file that ends neither in `.sv`
nor in `.svh`; a `.svh` file that holds code (classes, functions, design elements) rather than
macros, types and parameters. Verilog files (`.v`, `.vh`) are not checked, and extensions are
compared without regard to case.

## Why

One extension for code makes files easy to find and tools easy to configure (style guide
FIL-2).

## Example

```systemverilog bad
package p;
  `include "bus_item.svh"
endpackage
//@ file: bus_item.svh
class bus_item;
endclass
```

```systemverilog good
package p;
  `include "bus_defs.svh"
endpackage
//@ file: bus_defs.svh
`define BUS_WIDTH 32
```
"#;

const NAMING_CASE_PAGE: &str = r#"## What it finds

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
"#;

const NAMING_PREFIX_PAGE: &str = r#"## What it finds

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
"#;

const WEIGHT_GOAL_PAGE: &str = r#"## What it finds

`option.weight` or `option.goal` set in a covergroup, coverpoint or cross, or through a
covergroup handle, to anything but its default: `weight` 1 and `goal` 100, in any literal form
(`32'd1`), are not reported. `option.weight = 0` on a coverpoint that a cross of its covergroup
names (one that exists to build the cross) is the guide's exception and is not reported.

## Why

A goal under 100 hides holes, and weights turn the total into a negotiation (style guide
COV-7).

## Example

```systemverilog bad
module m;
  bit [1:0] mode;
  covergroup cg;
    option.goal = 90;
    coverpoint mode;
  endgroup
endmodule
```

```systemverilog good
module m;
  bit [1:0] mode;
  covergroup cg;
    coverpoint mode;
  endgroup
endmodule
```
"#;

const FORMATTER_DRIFT_PAGE: &str = r#"## What it finds

When `.dvicode/lint.json` says `"formatter": "dvicode"`: the lines that DVICode's formatter,
with the project's `.dvicode/format.json`, would change (one finding per run of lines).

## Why

A project that names its formatter keeps its code as the formatter makes it (style guide
FMT-13). Put the formatter's options in `.dvicode/format.json` so that every editor and the
CLI use the same ones.

## Example

```systemverilog bad
module m;
wire a;
endmodule
//@ file: .dvicode/lint.json
{ "formatter": "dvicode" }
```

```systemverilog good
module m;
  wire a;
endmodule
//@ file: .dvicode/lint.json
{ "formatter": "dvicode" }
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

/// A page as it is published: an example's `//@ file: NAME` lines become a block per file,
/// named, and `@@EOF` (the file ends without a newline) a note under its block.
pub fn render(page: &str) -> String {
    let mut out = String::new();
    let mut lines = page.lines();
    while let Some(l) = lines.next() {
        let tag = match l.trim() {
            "```systemverilog bad" => "bad",
            "```systemverilog good" => "good",
            _ => {
                out.push_str(l);
                out.push('\n');
                continue;
            }
        };
        let mut code = String::new();
        for c in lines.by_ref() {
            if c.trim() == "```" {
                break;
            }
            code.push_str(c);
            code.push('\n');
        }
        let files = example_files(&code);
        let named = files.len() > 1 || files.first().is_some_and(|(n, _)| n != "ex.sv");
        for (i, (name, text)) in files.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            if named {
                out.push_str(&format!("`{name}`:\n\n"));
            }
            let lang = if name.ends_with(".json") { "json" } else { "systemverilog" };
            out.push_str(&format!("```{lang} {tag}\n"));
            let no_newline = text.lines().any(|x| x.trim() == "@@EOF");
            for x in text.lines().filter(|x| x.trim() != "@@EOF") {
                out.push_str(x);
                out.push('\n');
            }
            out.push_str("```\n");
            if no_newline {
                out.push_str("\n(The file ends without a newline after its last line.)\n");
            }
        }
    }
    out
}

/// The files of an example: `//@ file: <name>` lines start each file; without one the code is
/// `ex.sv`.
pub fn example_files(code: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in code.split_inclusive('\n') {
        if let Some(name) = line.trim().strip_prefix("//@ file:") {
            out.push((name.trim().to_string(), String::new()));
            continue;
        }
        match out.last_mut() {
            Some((_, c)) => c.push_str(line),
            None => out.push(("ex.sv".to_string(), line.to_string())),
        }
    }
    out
}

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
