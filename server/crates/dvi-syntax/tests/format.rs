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
    assert_eq!(fmt(MESSY, &Options::default()), MESSY_FORMATTED, "blocks, if/else, case items and bodies, fork, headers, a function, directives kept, a define and a block comment untouched, UVM macro pairs, a constraint and a covergroup");
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
