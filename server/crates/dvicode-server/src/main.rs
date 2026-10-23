//! dvicode-server: SystemVerilog language server.
//!
//! Usage:
//!   dvicode-server [--stdio]                    LSP over stdin/stdout (default)
//!   dvicode-server --listen HOST:PORT [--token T] LSP over TCP (one client), token-protected
//!   dvicode-server check <dir> [--build FILE] [-q]  compile a project and print problems
//!   dvicode-server --version

mod check;
mod crash;
mod fs;
mod fuzz;
mod logger;
mod server;
mod transport;
mod util;
mod writes;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// musl's allocator serialises the threads that build a project; mimalloc does not.
#[cfg(all(target_env = "musl", target_arch = "x86_64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    crash::install_hook();
    // Parsing and analysis run on these threads; the default 2 MB stack is too small for
    // deeply nested code.
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(16 << 20)
        .thread_name(|i| format!("dvicode-worker-{i}"))
        .build_global();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log_level = log::LevelFilter::Info;
    let mut log_file: Option<String> = None;
    let mut listen: Option<String> = None;
    let mut token: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => {
                println!("dvicode-server {VERSION}");
                return;
            }
            "--help" | "-h" => {
                println!("{}", include_str!("usage.txt"));
                return;
            }
            "check" => {
                // `check ... --log-level debug` shows how the build file or the folder was read.
                let mut rest: Vec<String> = args[i + 1..].to_vec();
                let mut level = log::LevelFilter::Warn;
                if let Some(at) = rest.iter().position(|a| a == "--log-level") {
                    level = rest.get(at + 1).and_then(|l| l.parse().ok()).unwrap_or(log::LevelFilter::Debug);
                    rest.drain(at..(at + 2).min(rest.len()));
                }
                logger::init(level, None);
                std::process::exit(check::run(&rest));
            }
            "--stdio" => {}
            "--listen" | "--token" => {
                // A missing value must not quietly turn authentication off.
                let Some(v) = args.get(i + 1).filter(|v| !v.starts_with("--") && !v.is_empty()) else {
                    eprintln!("dvicode-server: {} needs a value (see --help)", args[i]);
                    std::process::exit(2);
                };
                if args[i] == "--listen" {
                    listen = Some(v.clone());
                } else {
                    token = Some(v.clone());
                }
                i += 1;
            }
            "--log-level" => {
                i += 1;
                log_level = args.get(i).and_then(|l| l.parse().ok()).unwrap_or(log::LevelFilter::Info);
            }
            "--log" => {
                i += 1;
                log_file = args.get(i).cloned();
            }
            other => {
                eprintln!("dvicode-server: unknown argument '{other}' (see --help)");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    logger::init(log_level, log_file.as_deref());
    let token = token.or_else(|| std::env::var("DVICODE_TOKEN").ok());
    let (conn, io_threads) = match listen {
        Some(addr) => match transport::listen(&addr, token.as_deref()) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("dvicode-server: {e}");
                std::process::exit(1);
            }
        },
        None => {
            let (c, t) = lsp_server::Connection::stdio();
            (c, transport::IoThreads::Stdio(t))
        }
    };
    log::info!("dvicode-server {VERSION} started (pid {})", std::process::id());
    if let Err(e) = server::run(conn) {
        log::error!("server error: {e}");
    }
    io_threads.join();
    log::info!("dvicode-server exiting");
}
