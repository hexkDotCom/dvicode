//! What lint rules know about the UVM library: its phases and its deprecated API.

/// A UVM phase method: its name and whether it is a task (time-consuming) or a function.
pub const PHASES: &[(&str, bool)] = &[
    ("build_phase", false),
    ("connect_phase", false),
    ("end_of_elaboration_phase", false),
    ("start_of_simulation_phase", false),
    ("run_phase", true),
    ("pre_reset_phase", true),
    ("reset_phase", true),
    ("post_reset_phase", true),
    ("pre_configure_phase", true),
    ("configure_phase", true),
    ("post_configure_phase", true),
    ("pre_main_phase", true),
    ("main_phase", true),
    ("post_main_phase", true),
    ("pre_shutdown_phase", true),
    ("shutdown_phase", true),
    ("post_shutdown_phase", true),
    ("extract_phase", false),
    ("check_phase", false),
    ("report_phase", false),
    ("final_phase", false),
];

pub fn phase(name: &str) -> Option<bool> {
    PHASES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// A deprecated part of the UVM API: deprecated in UVM 1.2 and removed in IEEE 1800.2.
pub struct Deprecated {
    /// The name as written.
    pub name: &'static str,
    /// The UVM class that declares it (`None`: a package item or a macro).
    pub owner: Option<&'static str>,
    pub macro_: bool,
    pub instead: &'static str,
}

pub const DEPRECATED: &[Deprecated] = &[
    Deprecated { name: "set_config_int", owner: Some("uvm_component"), macro_: false, instead: "uvm_config_db#(uvm_bitstream_t)::set" },
    Deprecated { name: "set_config_string", owner: Some("uvm_component"), macro_: false, instead: "uvm_config_db#(string)::set" },
    Deprecated { name: "set_config_object", owner: Some("uvm_component"), macro_: false, instead: "uvm_config_db#(uvm_object)::set" },
    Deprecated { name: "get_config_int", owner: Some("uvm_component"), macro_: false, instead: "uvm_config_db#(uvm_bitstream_t)::get" },
    Deprecated { name: "get_config_string", owner: Some("uvm_component"), macro_: false, instead: "uvm_config_db#(string)::get" },
    Deprecated { name: "get_config_object", owner: Some("uvm_component"), macro_: false, instead: "uvm_config_db#(uvm_object)::get" },
    Deprecated { name: "set_config_int", owner: None, macro_: false, instead: "uvm_config_db#(uvm_bitstream_t)::set" },
    Deprecated { name: "set_config_string", owner: None, macro_: false, instead: "uvm_config_db#(string)::set" },
    Deprecated { name: "set_config_object", owner: None, macro_: false, instead: "uvm_config_db#(uvm_object)::set" },
    Deprecated { name: "print_config_settings", owner: Some("uvm_component"), macro_: false, instead: "print_config" },
    Deprecated { name: "global_stop_request", owner: None, macro_: false, instead: "objections (raise_objection/drop_objection)" },
    Deprecated { name: "stop_request", owner: Some("uvm_root"), macro_: false, instead: "objections (raise_objection/drop_objection)" },
    Deprecated { name: "set_global_timeout", owner: None, macro_: false, instead: "uvm_root::set_timeout" },
    Deprecated { name: "set_global_stop_timeout", owner: None, macro_: false, instead: "objections and phase drain times" },
    Deprecated { name: "uvm_test_done", owner: None, macro_: false, instead: "the phase's objection (phase.raise_objection)" },
    Deprecated { name: "starting_phase", owner: Some("uvm_sequence_base"), macro_: false, instead: "get_starting_phase() / set_starting_phase()" },
    Deprecated { name: "uvm_sequence_utils", owner: None, macro_: true, instead: "`uvm_object_utils and `uvm_declare_p_sequencer" },
    Deprecated { name: "uvm_sequence_utils_begin", owner: None, macro_: true, instead: "`uvm_object_utils_begin and `uvm_declare_p_sequencer" },
    Deprecated { name: "uvm_sequencer_utils", owner: None, macro_: true, instead: "`uvm_component_utils" },
    Deprecated { name: "uvm_sequencer_utils_begin", owner: None, macro_: true, instead: "`uvm_component_utils_begin" },
    Deprecated { name: "uvm_sequencer_param_utils", owner: None, macro_: true, instead: "`uvm_component_param_utils" },
    Deprecated { name: "uvm_update_sequence_lib", owner: None, macro_: true, instead: "uvm_sequence_library" },
    Deprecated { name: "uvm_update_sequence_lib_and_item", owner: None, macro_: true, instead: "uvm_sequence_library" },
    Deprecated { name: "uvm_declare_sequence_lib", owner: None, macro_: true, instead: "uvm_sequence_library" },
];

/// The entries for a name (a member may be deprecated both in a class and as a package item).
pub fn deprecated(name: &str, macro_: bool) -> impl Iterator<Item = &'static Deprecated> + '_ {
    DEPRECATED.iter().filter(move |d| d.name == name && d.macro_ == macro_)
}
