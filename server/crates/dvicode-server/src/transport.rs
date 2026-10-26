//! TCP transport with a token handshake (stdio comes from lsp-server).

use crossbeam_channel::{bounded, Receiver, Sender};
use lsp_server::{Connection, Message};
use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Compares in time that depends only on the lengths, not on where the bytes differ.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// An accept error that concerns only the client that went away (it reset the connection
/// before it was accepted): the server keeps listening.
fn one_client(e: &std::io::Error) -> bool {
    // Linux also passes on the network errors of a pending connection (accept(2)): EPROTO,
    // ENOPROTOOPT, EHOSTDOWN, ENONET, EOPNOTSUPP.
    const LINUX: &[i32] = &[71, 92, 112, 64, 95];
    matches!(
        e.kind(),
        ErrorKind::Interrupted
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::TimedOut
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkUnreachable
            | ErrorKind::NetworkDown
    ) || cfg!(target_os = "linux") && e.raw_os_error().is_some_and(|n| LINUX.contains(&n))
}

/// Handshakes still running at once; more connections are closed straight away.
const MAX_PENDING: usize = 16;
/// Time a client has to send its whole token line.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// The first connection that sends the token. Each handshake runs on its own thread with a
/// deadline for the whole line, so a client that sends nothing, or sends slowly, holds up
/// no one else.
fn accept_with_token(listener: &TcpListener, token: &str) -> Result<(TcpStream, SocketAddr), String> {
    let expected = Arc::new(format!("DVICODE-TOKEN {token}").into_bytes());
    let pending = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = bounded::<(TcpStream, SocketAddr)>(MAX_PENDING);
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    loop {
        if let Ok(won) = rx.recv_timeout(Duration::from_millis(50)) {
            let _ = listener.set_nonblocking(false);
            return Ok(won);
        }
        let (stream, peer) = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.kind() == ErrorKind::WouldBlock || one_client(&e) => continue,
            // A handshake that has just succeeded still wins.
            Err(e) => return rx.try_recv().map_err(|_| e.to_string()),
        };
        if pending.load(Ordering::SeqCst) >= MAX_PENDING || stream.set_nonblocking(false).is_err() {
            log::warn!("rejected client {peer}: too many handshakes");
            continue;
        }
        pending.fetch_add(1, Ordering::SeqCst);
        let (expected, pending, tx) = (expected.clone(), pending.clone(), tx.clone());
        thread::spawn(move || {
            if read_token_line(&stream).is_some_and(|line| same_secret(&line, &expected)) {
                let _ = tx.try_send((stream, peer));
            } else {
                log::warn!("rejected client {peer}: bad token");
            }
            pending.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// The first line, without its end, read a byte at a time (nothing after it is consumed)
/// within [`HANDSHAKE`]; `None` past 512 bytes, on a timeout or a closed connection.
fn read_token_line(mut stream: &TcpStream) -> Option<Vec<u8>> {
    let deadline = Instant::now() + HANDSHAKE;
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while line.len() < 512 {
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut b) {
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Ok(1) if b[0] == b'\n' => {
                stream.set_read_timeout(None).ok()?;
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Some(line);
            }
            Ok(1) => line.push(b[0]),
            _ => return None,
        }
    }
    None
}

pub enum IoThreads {
    Stdio(lsp_server::IoThreads),
    /// Reader, writer, and the socket (shut down between them).
    Tcp(thread::JoinHandle<()>, thread::JoinHandle<()>, std::net::TcpStream),
}

impl IoThreads {
    pub fn join(self) {
        match self {
            IoThreads::Stdio(t) => {
                let _ = t.join();
            }
            IoThreads::Tcp(r, w, s) => {
                // The writer ends with the server's last message; the reader may wait for a
                // client that never sends `exit`.
                let _ = w.join();
                let _ = s.shutdown(std::net::Shutdown::Both);
                let _ = r.join();
            }
        }
    }
}

/// Accepts one client on `addr`. With a token, the first line from the client must be
/// `DVICODE-TOKEN <token>`; other clients are rejected and the server keeps listening. Without
/// one, only a loopback address is accepted.
pub fn listen(addr: &str, token: Option<&str>) -> Result<(Connection, IoThreads), String> {
    let listener = TcpListener::bind(addr).map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    if token.is_none() && !local.ip().to_canonical().is_loopback() {
        // Whoever connected first could read and write every file this account can.
        return Err(format!("refusing to listen on {local} without a token: set DVICODE_TOKEN, or listen on 127.0.0.1 and tunnel"));
    }
    // The client (or a wrapper script) reads this line to learn the port.
    eprintln!("DVICODE-LISTENING {local}");
    log::info!("listening on {local}");
    if token.is_none() {
        log::warn!("no token: any program on this machine can connect to {local}; set DVICODE_TOKEN");
    }
    let (stream, peer) = match token {
        None => loop {
            match listener.accept() {
                Ok(c) => break c,
                Err(e) if one_client(&e) => {}
                Err(e) => return Err(e.to_string()),
            }
        },
        Some(t) => accept_with_token(&listener, t)?,
    };
    let reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let shut = stream.try_clone().map_err(|e| e.to_string())?;
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
    Ok((Connection { sender: writer_tx, receiver: reader_rx }, IoThreads::Tcp(r, w, shut)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn no_token_needs_a_loopback_address() {
        let e = listen("0.0.0.0:0", None).err().unwrap();
        assert!(e.contains("without a token"), "{e}");
    }

    #[test]
    fn a_silent_client_does_not_hold_up_the_one_with_the_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // One client connects and says nothing, one sends a wrong token, then the right one,
        // with the first message right behind its token line.
        let _silent = TcpStream::connect(addr).unwrap();
        let mut wrong = TcpStream::connect(addr).unwrap();
        wrong.write_all(b"DVICODE-TOKEN nope\n").unwrap();
        let mut right = TcpStream::connect(addr).unwrap();
        right.write_all(b"DVICODE-TOKEN s3cret\r\nHELLO").unwrap();
        let t = Instant::now();
        let (stream, _) = accept_with_token(&listener, "s3cret").unwrap();
        assert!(t.elapsed() < HANDSHAKE, "{:?}", t.elapsed());
        assert_eq!(stream.peer_addr().unwrap(), right.local_addr().unwrap());
        // Nothing after the token line was consumed.
        let mut rest = [0u8; 5];
        (&stream).read_exact(&mut rest).unwrap();
        assert_eq!(&rest, b"HELLO");
    }
}
