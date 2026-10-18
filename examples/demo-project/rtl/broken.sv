// Deliberate mistakes; each line marked "error:" must be reported.
module broken (input logic clk);
  import demo_pkg::*;

  state_e st;
  req_t   r;
  stat_e u;                       // error: unknown type (typo of state_e)

  initial begin
    st = IDLE;
    r.adr = 1;                    // error: no member 'adr'
    undeclared_var = 2;           // error: undeclared
    st = BUSY                     // error: missing ';'
  end
endmodule

module broken2;
  import no_such_pkg::*;          // error: unknown package
  `include "does_not_exist.svh"   // error: missing include
  initial `UNDEFINED_MACRO(1);    // error: undefined macro
  fifo u_fifo (.clk(1'b0), .bogus(1'b1));  // error: no port 'bogus'
endmodule

module broken3;
  req_t r;                        // error: unknown type (needs import demo_pkg::*)
endmodule
