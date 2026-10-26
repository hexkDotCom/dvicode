use dvi_syntax::format::{apply, candidate, difference, format, range_lines, Directives, Options};

/// `src` formatted with `o`, checked to be idempotent; a refusal shows the candidate.
fn fmt(src: &str, o: &Options) -> String {
    match format(src, o) {
        Some(f) => {
            let out = apply(src, &f, None);
            let again = format(&out, o).expect("formats its own result");
            assert!(again.edits.is_empty(), "not idempotent: {:?}\n{out}", again.edits);
            out
        }
        None => {
            let out = apply(src, &candidate(src, o), None);
            panic!("refused: {:?}\n{out}", difference(src, &out, false));
        }
    }
}

const MESSY: &str = "module top #(
parameter W = 8,
    parameter D = 4
) (
input logic clk,
      output logic [W-1:0] q
);
logic a, b;
   always_ff @(posedge clk) begin
if (a)
q <= 0;
else if (b)
q <= 1;
     else begin
q <= q + 1;
end
end
always_comb
case (a)
0: b = 1;
1: begin
b = 0;
end
default:
b = 'x;
endcase
initial begin
fork
#1 a = 1;
join_none
wait fork;
end
function automatic int f(input int x);
return x +
1;
endfunction
`ifdef SIM
initial $display(\"sim\");
`else
initial $display(\"no\");
`endif
`define M(x) \\
  x + \\
    1
/* a block
     comment */
assign a = b; // trailing
endmodule
class c extends base;
`uvm_object_utils_begin(c)
`uvm_field_int(x, UVM_ALL_ON)
`uvm_object_utils_end
`uvm_object_new
rand int x;
extern function void f();
constraint k {
x < 10;
}
covergroup cg @(posedge clk);
cp: coverpoint x {
bins b = {1};
}
endgroup
endclass
";

const MESSY_FORMATTED: &str = "module top #(
  parameter W = 8,
  parameter D = 4
) (
  input logic clk,
  output logic [W-1:0] q
);
  logic a, b;
  always_ff @(posedge clk) begin
    if (a)
      q <= 0;
    else if (b)
      q <= 1;
    else begin
      q <= q + 1;
    end
  end
  always_comb
    case (a)
      0: b = 1;
      1: begin
        b = 0;
      end
      default:
        b = 'x;
    endcase
  initial begin
    fork
      #1 a = 1;
    join_none
    wait fork;
  end
  function automatic int f(input int x);
    return x +
      1;
  endfunction
`ifdef SIM
  initial $display(\"sim\");
`else
  initial $display(\"no\");
`endif
`define M(x) \\
  x + \\
    1
  /* a block
     comment */
  assign a = b; // trailing
endmodule
class c extends base;
  `uvm_object_utils_begin(c)
    `uvm_field_int(x, UVM_ALL_ON)
  `uvm_object_utils_end
  `uvm_object_new
  rand int x;
  extern function void f();
  constraint k {
    x < 10;
  }
  covergroup cg @(posedge clk);
    cp: coverpoint x {
      bins b = {1};
    }
  endgroup
endclass
";

#[test]
fn indentation_follows_the_constructs() {
    let got = fmt(MESSY, &Options::default());
    let diff: Vec<String> = got.lines().zip(MESSY_FORMATTED.lines()).enumerate().filter(|(_, (g, w))| g != w).map(|(i, (g, w))| format!("{}: {g:?} want {w:?}", i + 1)).collect();
    assert!(diff.is_empty() && got == MESSY_FORMATTED, "blocks, if/else, case items and bodies, fork, headers, a function, directives kept, a define and a block comment untouched, UVM macro pairs, a constraint and a covergroup:\n{}", diff.join("\n"));
}

#[test]
fn options_change_the_style() {
    let src = "module m;\nalways_comb\ncase (a)\n0: b = 1;\nendcase\n`ifdef X\nlogic x;\n`endif\nendmodule\n";
    let o = Options { indent_size: 4, use_tabs: true, indent_case_items: false, directives: Directives::Indent, ..Options::default() };
    assert_eq!(fmt(src, &o), "module m;\n\talways_comb\n\t\tcase (a)\n\t\t0: b = 1;\n\t\tendcase\n\t`ifdef X\n\tlogic x;\n\t`endif\nendmodule\n");
    let o = Options { directives: Directives::Column0, indent_size: 3, ..Options::default() };
    assert_eq!(fmt("module m;\n  `ifdef X\n  logic x;\n  `endif\nendmodule\n", &o), "module m;\n`ifdef X\n   logic x;\n`endif\nendmodule\n");
    // Blank lines: kept by default, limited when asked.
    let src = "module m;\n\n\n\nlogic a;\nendmodule\n";
    assert_eq!(fmt(src, &Options::default()), "module m;\n\n\n\n  logic a;\nendmodule\n");
    assert_eq!(fmt(src, &Options { max_blank_lines: 1, ..Options::default() }), "module m;\n\n  logic a;\nendmodule\n");
    // Lists aligned after their `(`.
    let src = "module m;\nfoo u (.a(x),\n.b(y));\nendmodule\n";
    assert_eq!(fmt(src, &Options { align_lists: true, ..Options::default() }), "module m;\n  foo u (.a(x),\n         .b(y));\nendmodule\n");
    assert_eq!(fmt("module m;\n  foo u (.a(x),\n         .b(y));\nendmodule\n", &Options::default()), "module m;\n  foo u (.a(x),\n         .b(y));\nendmodule\n", "aligned by hand after the `(`: kept");
}

#[test]
fn hand_alignment_is_kept_and_moved() {
    // A continuation aligned under a token of the line above moves with its statement.
    let src = "module m;\n    assign x = c ? a\n                 : b;\nendmodule\n";
    assert_eq!(fmt(src, &Options::default()), "module m;\n  assign x = c ? a\n               : b;\nendmodule\n");
    // A comment continuing the trailing comment of the line above stays under it.
    let src = "module m;\n  logic a;   // first\n             // second\nendmodule\n";
    assert_eq!(fmt(src, &Options::default()), src);
    // `begin` on the line after `if`, at the `if`'s level or one deeper: both kept.
    let src = "module m;\n  always_comb\n    if (a)\n    begin\n      b = 1;\n    end\nendmodule\n";
    assert_eq!(fmt(src, &Options::default()), src);
    let src = "module m;\n  always_comb\n    if (a)\n      begin\n        b = 1;\n      end\nendmodule\n";
    assert_eq!(fmt(src, &Options::default()), src);
    // A multi-line condition: the block goes by the `if`.
    let src = "module m;\nalways_comb\nif (a &&\nb) begin\nc = 1;\nend\nendmodule\n";
    assert_eq!(fmt(src, &Options::default()), "module m;\n  always_comb\n    if (a &&\n      b) begin\n      c = 1;\n    end\nendmodule\n");
}

#[test]
fn what_whitespace_could_change_is_left_alone() {
    // A define (its continuation, a `//` comment ending in `\`), a string continued, a
    // protected envelope, a `\` before trailing blanks, an escaped identifier.
    let src = "module m;\n`define A(x) \\\n    x + \\\n // note \\\n      1\n    initial $display(\"a \\\n   b\");\n`pragma protect begin_protected\n   \"payload\" /* not a comment\n`pragma protect end_protected\n    logic \\esc ;\n    wire w = a \\   \nendmodule\n";
    let out = fmt(src, &Options::default());
    assert_eq!(out, "module m;\n`define A(x) \\\n    x + \\\n // note \\\n      1\n  initial $display(\"a \\\n   b\");\n`pragma protect begin_protected\n   \"payload\" /* not a comment\n`pragma protect end_protected\n  logic \\esc ;\n  wire w = a \\   \nendmodule\n");
    // CRLF, no final newline, non-ASCII text, an empty file.
    assert_eq!(fmt("module m;\r\nlogic a; // é\r\nendmodule", &Options::default()), "module m;\r\n  logic a; // é\r\nendmodule");
    assert_eq!(fmt("", &Options::default()), "");
    assert_eq!(fmt("\n\n", &Options::default()), "\n\n");
}

#[test]
fn the_check_refuses_more_than_whitespace() {
    assert!(difference("logic \\a ;", "logic a ;", false).is_some(), "an escaped identifier");
    assert!(difference("// one\nx;", "// two\nx;", false).is_some(), "a comment's text");
    assert!(difference("x; // c   \n", "x; // c\n", false).is_none(), "a line comment's trailing blanks");
    assert!(difference("`define F(x) x\n", "`define F (x) x\n", false).is_some(), "a macro's parameter list");
    assert!(difference("a\nb\n", "a\n\nb\n", false).is_some(), "a line added");
    assert!(difference("a\n\nb\n", "a\nb\n", true).is_none(), "a blank line removed when allowed");
}

#[test]
fn a_range_formats_the_lines_it_touches() {
    assert_eq!(range_lines(2, 4, 0), (2, 4), "an end at column 0 does not take its line");
    assert_eq!(range_lines(2, 4, 3), (2, 5));
    assert_eq!(range_lines(3, 3, 0), (3, 4), "an empty range: its line");
    let src = "module m;\nlogic a;\nlogic b;\nlogic c;\nendmodule\n";
    let f = format(src, &Options::default()).unwrap();
    assert_eq!(apply(src, &f, Some((2, 3))), "module m;\nlogic a;\n  logic b;\nlogic c;\nendmodule\n");
}

#[test]
fn incomplete_and_non_ascii_text_does_not_break_the_scanner() {
    // A lone backtick at the end, non-ASCII text in a triple-quoted string and after a `\`.
    for src in ["module m;\nlogic a; `", "module m;\nstring s = \"\"\"é\"\"\";\nendmodule\n", "module m;\nx = \"\\é\";\nendmodule\n", "`"] {
        let _ = format(src, &Options::default());
    }
}

#[test]
fn review_cases_of_the_formatter() {
    let o = Options::default();
    // An attribute before a module.
    assert_eq!(fmt("(* keep = \"true\" *)\nmodule m;\nlogic x;\nendmodule\n", &o), "(* keep = \"true\" *)\nmodule m;\n  logic x;\nendmodule\n");
    // `always @*` and `always #5` end their event control.
    assert_eq!(fmt("module m;\nalways @*\nbegin\nx = y;\nend\nalways #5\nclk = ~clk;\nendmodule\n", &o), "module m;\n  always @*\n  begin\n    x = y;\n  end\n  always #5\n    clk = ~clk;\nendmodule\n");
    // `if` and `foreach` in a constraint.
    assert_eq!(
        fmt("class c;\nrand int x;\nconstraint c1 {\nif (x > 0)\nx < 10;\nforeach (a[i]) {\na[i] < 4;\n}\n}\nendclass\n", &o),
        "class c;\n  rand int x;\n  constraint c1 {\n    if (x > 0)\n      x < 10;\n    foreach (a[i]) {\n      a[i] < 4;\n    }\n  }\nendclass\n"
    );
    // `do ... while` in an unbraced `if`.
    assert_eq!(fmt("module m;\ninitial begin\nif (a)\ndo\nx++;\nwhile (b);\nend\nendmodule\n", &o), "module m;\n  initial begin\n    if (a)\n      do\n        x++;\n      while (b);\n  end\nendmodule\n");
    // An assertion's `else` is the assertion's.
    assert_eq!(
        fmt("module m;\ninitial begin\nif (enable)\nassert (ready)\npass();\nelse\nfail();\nend\nendmodule\n", &o),
        "module m;\n  initial begin\n    if (enable)\n      assert (ready)\n        pass();\n      else\n        fail();\n  end\nendmodule\n"
    );
    // A string continued with `\` and a CRLF line break.
    assert_eq!(fmt("module m;\r\ninitial $display(\"a \\\r\n   b\");\r\nendmodule\r\n", &o), "module m;\r\n  initial $display(\"a \\\r\n   b\");\r\nendmodule\r\n");
    // A macro call's argument on a line at column 0 stays there (stringification).
    assert_eq!(fmt("`define S(x) `\"x`\"\nmodule m;\ninitial $display(`S(a\n+b));\nendmodule\n", &o), "`define S(x) `\"x`\"\nmodule m;\n  initial $display(`S(a\n+b));\nendmodule\n");
    // A long line: columns in one pass.
    let long = format!("module m;\nassign x = {};\nendmodule\n", vec!["a"; 100_000].join(" + "));
    let t0 = std::time::Instant::now();
    assert!(format(&long, &o).is_some());
    assert!(t0.elapsed().as_secs() < 20, "{:?}", t0.elapsed());
    // Astral characters before trailing blanks, no final newline.
    assert_eq!(fmt("module m;\nlogic a; // 😀   \nendmodule", &o), "module m;\n  logic a; // 😀\nendmodule");
    // An unterminated protected envelope: kept to the end.
    let src = "module m;\n`pragma protect begin_protected\n  x  \n   y\n";
    assert_eq!(fmt(src, &o), src);
}

#[test]
fn indentation_past_any_real_nesting_is_left_alone() {
    let n = 2_000;
    let src = format!("module m;\ninitial\n{}x = 1;\n{}endmodule\n", "begin\n".repeat(n), "end\n".repeat(n));
    let out = fmt(&src, &Options::default());
    let widest = out.lines().map(|l| l.len() - l.trim_start().len()).max().unwrap();
    assert!(widest <= 512, "{widest} columns");
}
