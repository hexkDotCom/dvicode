//! TCP transport with a token handshake (stdio comes from lsp-server).

use crossbeam_channel::{bounded, Receiver, Sender};
use lsp_server::{Connection, Message};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

pub enum IoThreads {
    Stdio(lsp_server::IoThreads),
    Tcp(Vec<thread::JoinHandle<()>>),
}

impl IoThreads {
    pub fn join(self) {
        match self {
            IoThreads::Stdio(t) => {
                let _ = t.join();
            }
            IoThreads::Tcp(v) => {
                for h in v {
                    let _ = h.join();
                }
            }
        }
    }
}

/// Accepts one client on `addr`. With a token, the first line from the client must be
/// `DVICODE-TOKEN <token>`; other clients are rejected and the server keeps listening.
pub fn listen(addr: &str, token: Option<&str>) -> Result<(Connection, IoThreads), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    // The client (or a wrapper script) reads this line to learn the port.
    eprintln!("DVICODE-LISTENING {local}");
    log::info!("listening on {local}");
    loop {
        let (stream, peer) = listener.accept().map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        if let Some(t) = token {
            let mut line = String::new();
            let ok = reader.read_line(&mut line).is_ok() && line.trim() == format!("DVICODE-TOKEN {t}");
            if !ok {
                log::warn!("rejected client {peer}: bad token");
                continue;
            }
        }
        log::info!("client connected from {peer}");
        let (reader_tx, reader_rx): (Sender<Message>, Receiver<Message>) = bounded(0);
        let r = thread::spawn(move || {
            let mut reader = reader;
            while let Ok(Some(msg)) = Message::read(&mut reader) {
                let exit = matches!(&msg, Message::Notification(n) if n.method == "exit");
                if reader_tx.send(msg).is_err() || exit {
                    break;
                }
            }
        });
        let (writer_tx, writer_rx): (Sender<Message>, Receiver<Message>) = bounded(0);
        let mut out = stream;
        let w = thread::spawn(move || {
            for msg in writer_rx {
                if msg.write(&mut out).is_err() {
                    break;
                }
                let _ = out.flush();
            }
        });
        return Ok((Connection { sender: writer_tx, receiver: reader_rx }, IoThreads::Tcp(vec![r, w])));
    }
}
