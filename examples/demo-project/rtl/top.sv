// Design top: a controller in front of a FIFO.
module top (
  input logic clk,
  input logic rst_n
);
  import demo_pkg::*;

  state_e state;
  req_t   req;
  logic   full, empty;
  logic [WIDTH-1:0] data;

  fifo #(.W(WIDTH), .DEPTH(8)) u_fifo (
    .clk  (clk),
    .rst_n(rst_n),
    .push (state == BUSY),
    .pop  (1'b0),
    .din  (req.data),
    .dout (data),
    .full (full),
    .empty(empty)
  );

  generate
    for (genvar i = 0; i < 2; i++) begin : g_mon
      fifo #(.W(8)) u_mon (.clk(clk), .rst_n(rst_n), .push(1'b0), .pop(1'b0), .din('0), .dout(), .full(), .empty());
    end
  endgenerate

  always_ff @(posedge clk) begin
    case (state)
      IDLE: state <= BUSY;
      BUSY: if (full) state <= DONE;
      default: state <= IDLE;
    endcase
  end
endmodule
