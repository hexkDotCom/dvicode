// Simple synchronous FIFO.
module fifo #(
  parameter int W     = 8,
  parameter int DEPTH = 4
) (
  input  logic         clk,
  input  logic         rst_n,
  input  logic         push,
  input  logic         pop,
  input  logic [W-1:0] din,
  output logic [W-1:0] dout,
  output logic         full,
  output logic         empty
);
  localparam int AW = demo_pkg::clog2(DEPTH);

  logic [W-1:0]  mem [DEPTH];
  logic [AW:0]   count;
  logic [AW-1:0] rd_ptr, wr_ptr;

  assign full  = (count == DEPTH);
  assign empty = (count == 0);
  assign dout  = mem[rd_ptr];

  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      count  <= '0;
      rd_ptr <= '0;
      wr_ptr <= '0;
    end else begin
      if (push && !full) begin
        mem[wr_ptr] <= din;
        wr_ptr <= wr_ptr + 1;
      end
      if (pop && !empty) rd_ptr <= rd_ptr + 1;
      count <= count + (push && !full) - (pop && !empty);
    end
  end
endmodule
