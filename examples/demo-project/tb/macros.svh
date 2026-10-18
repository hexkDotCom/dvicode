`ifndef DEMO_MACROS_SVH
`define DEMO_MACROS_SVH

// Prints a message with the simulation time.
`define LOG(msg) $display("[%0t] %s", $time, msg)

// Checks a condition and reports an error with an id.
`define CHECK(id, cond) \
  if (!(cond)) $error("%s: check failed: %s", id, `"cond`")

`endif
