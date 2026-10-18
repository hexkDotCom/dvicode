// Testbench top.
module tb_top;
  import tb_pkg::*;

  logic clk = 0;
  logic rst_n;

  always #5 clk = ~clk;

  top dut (.clk(clk), .rst_n(rst_n));

  initial begin
    driver  drv = new();
    my_item it  = new();
    rst_n = 0;
    #20 rst_n = 1;
    void'(it.randomize() with { addr > 10; });
    drv.mbx.put(it);
    fork
      drv.run();
    join_none
    #100 $finish;
  end
endmodule
