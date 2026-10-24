// Verification classes.
package tb_pkg;
  `include "macros.svh"

  // Base transaction.
  class base_item;
    rand bit [7:0] addr;

    constraint c_addr { addr < 100; }

    function new();
    endfunction

    // Text form of the item.
    virtual function string convert2string();
      return $sformatf("addr=%0d", addr);
    endfunction
  endclass

  // Transaction with data.
  class my_item extends base_item;
    rand bit [15:0] data;
    bit [7:0] history[$];

    extern function void print();

    virtual function string convert2string();
      return {super.convert2string(), $sformatf(" data=%0h", data)};
    endfunction
  endclass

  function void my_item::print();
    `LOG(convert2string());
    history.push_back(addr);
  endfunction

  // Drives items from a mailbox.
  class driver;
    mailbox #(my_item) mbx;
    int unsigned count;

    function new();
      mbx = new();
    endfunction

    task run();
      my_item it;
      forever begin
        mbx.get(it);
        it.print();
        count++;
        `CHECK("DRV", it.addr < 100);
      end
    endtask
  endclass

  // Coverage of the items driven: what samples `addr` (Show Assertions and Coverage).
  class item_coverage;
    covergroup cg_addr with function sample(bit [7:0] a);
      cp_addr: coverpoint a {
        bins low  = {[0:49]};
        bins high = {[50:99]};
      }
    endgroup
    function new();
      cg_addr = new();
    endfunction
    function void sample(my_item it);
      cg_addr.sample(it.addr);
    endfunction
  endclass
endpackage
