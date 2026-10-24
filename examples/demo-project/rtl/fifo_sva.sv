// Assertions on the FIFO, bound to every instance of it. Right-click `push` or `full` in
// fifo.sv and choose Show Assertions and Coverage.
module fifo_sva (
  input logic clk,
  input logic rst_n,
  input logic push,
  input logic pop,
  input logic full,
  input logic empty
);
  default clocking cb @(posedge clk); endclocking
  default disable iff (!rst_n);

  property p_no_overflow;
    full |-> !push;
  endproperty

  a_no_overflow: assert property (p_no_overflow) else $error("push while full");
  a_no_underflow: assert property (empty |-> !pop) else $error("pop while empty");
  c_full: cover property (full);
endmodule

bind fifo fifo_sva u_sva (.clk(clk), .rst_n(rst_n), .push(push), .pop(pop), .full(full), .empty(empty));
