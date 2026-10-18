// Built-in package std (IEEE 1800-2017 clause 18 and 15), implicitly imported everywhere.
package std;

  // Semaphore (15.3).
  class semaphore;
    extern function new(int keyCount = 0);
    extern function void put(int keyCount = 1);
    extern task get(int keyCount = 1);
    extern function int try_get(int keyCount = 1);
  endclass

  // Mailbox (15.4).
  class mailbox #(type T = int);
    extern function new(int bound = 0);
    extern function int num();
    extern task put(T message);
    extern function int try_put(T message);
    extern task get(ref T message);
    extern function int try_get(ref T message);
    extern task peek(ref T message);
    extern function int try_peek(ref T message);
  endclass

  // Process control (9.7).
  class process;
    typedef enum { FINISHED, RUNNING, WAITING, SUSPENDED, KILLED } state;
    extern static function process self();
    extern function state status();
    extern function void kill();
    extern task await();
    extern function void suspend();
    extern function void resume();
    extern function void srandom(int seed);
    extern function string get_randstate();
    extern function void set_randstate(string state);
  endclass

  // Scope randomize (18.12).
  extern function int randomize();

endpackage
