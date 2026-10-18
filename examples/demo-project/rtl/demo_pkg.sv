// Types and helpers shared by the RTL.
package demo_pkg;

  // Controller state.
  typedef enum logic [1:0] {
    IDLE,
    BUSY,
    DONE
  } state_e;

  /** A bus request. */
  typedef struct packed {
    logic [7:0]  addr;
    logic [15:0] data;
  } req_t;

  parameter int WIDTH = `DEMO_WIDTH;

  // Ceiling of log2.
  function automatic int clog2(int value);
    int r = 0;
    while ((1 << r) < value) r++;
    return r;
  endfunction

endpackage
