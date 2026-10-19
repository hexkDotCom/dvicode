//! TCP transport with a token handshake (stdio comes from lsp-server).

use crossbeam_channel::{bounded, Receiver, Sender};
use lsp_server::{Connection, Message};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

/// Compares in time that depends only on the lengths, not on where the bytes differ.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

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
    if token.is_none() {
        // Whoever connects first can read and write every file this account can.
        log::warn!("no token: any program that can reach {local} can connect; set DVICODE_TOKEN");
    }
    loop {
        let (stream, peer) = listener.accept().map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        if let Some(t) = token {
            // A client that sends nothing, or an endless line, must not hold up the others.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut line = String::new();
            let read = (&mut reader).take(512).read_line(&mut line).is_ok();
            let ok = read && same_secret(line.trim_end_matches(['\r', '\n']).as_bytes(), format!("DVICODE-TOKEN {t}").as_bytes());
            if !ok {
                log::warn!("rejected client {peer}: bad token");
                continue;
            }
            let _ = stream.set_read_timeout(None);
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
