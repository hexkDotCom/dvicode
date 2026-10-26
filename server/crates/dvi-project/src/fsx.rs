//! The file system, as the engine may touch it: every read, listing and look at a path of a
//! project goes through here, so that a network path to a host the user has not allowed is
//! never reached (see [`crate::netpath`]), whether it is written as such or reached through a
//! symbolic link or junction on the way. Such a path is as if it could not be read.

use crate::netpath;
use rustc_hash::FxHashMap;
use std::fs::Metadata;
use std::io::{Error, ErrorKind, Result};
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;

/// What is known of folders on the way to paths: cleared at each build and when the allowed
/// hosts change.
static FOLDERS: RwLock<Option<FxHashMap<PathBuf, Seen>>> = RwLock::new(None);

#[derive(Clone)]
enum Seen {
    /// Not a link.
    Plain,
    /// A link (symbolic link or junction) to this absolute path.
    Link(PathBuf),
    /// A link that cannot be read: it may lead anywhere.
    Unknown,
}

/// Links followed from one path at most: as many as Windows follows.
const MAX_LINKS: usize = 63;

/// Forgets what is known of folders: a build starts, or the allowed hosts changed.
pub fn forget() {
    *FOLDERS.write().unwrap() = None;
}

/// The host a path leads to that is not allowed: written in it, or the target of a link on the
/// way (an empty name when only a link leads there, or a link that cannot be read).
pub fn blocked(path: impl AsRef<Path>) -> Option<String> {
    blocked_within(path.as_ref(), MAX_LINKS, true)
}

/// Whether reaching `path` may sign in to a network host that is not allowed. Links are looked
/// at only on Windows, where a link to `\\host\share` is followed by the system; `leaf`: the path
/// itself too, not only its folders.
fn blocked_within(path: &Path, links: usize, leaf: bool) -> Option<String> {
    if let Some(h) = netpath::blocked(path) {
        return Some(h);
    }
    if !cfg!(windows) || !netpath::restricted() {
        return None;
    }
    // As Windows reads it: `..` taken away by name, before any link is followed.
    let path = lexical(path);
    let parts: Vec<Component> = path.components().collect();
    let mut cur = PathBuf::new();
    for (i, c) in parts.iter().enumerate() {
        cur.push(c);
        if !matches!(c, Component::Normal(_)) {
            continue;
        }
        let last = i + 1 == parts.len();
        if last && !leaf {
            break;
        }
        let known = if last { None } else { FOLDERS.read().unwrap().as_ref().and_then(|m| m.get(&cur).cloned()) };
        let seen = match known {
            Some(s) => s,
            None => {
                let Some(s) = look(&cur) else {
                    // Not there: nothing to follow (and nothing to remember, it may come).
                    continue;
                };
                if !last {
                    FOLDERS.write().unwrap().get_or_insert_with(FxHashMap::default).insert(cur.clone(), s.clone());
                }
                s
            }
        };
        match seen {
            Seen::Plain => {}
            Seen::Unknown => return Some(String::new()),
            Seen::Link(_) if links == 0 => return Some(String::new()),
            Seen::Link(target) => {
                // The system goes on from where the link leads: its target, then the rest.
                let mut next = target;
                next.extend(&parts[i + 1..]);
                return blocked_within(&next, links - 1, leaf);
            }
        }
    }
    None
}

/// A path looked at without following it: `None` when there is nothing there.
fn look(path: &Path) -> Option<Seen> {
    let m = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => return None,
        // Cannot be looked at, so cannot be known not to lead elsewhere.
        Err(_) => return Some(Seen::Unknown),
    };
    // Symbolic links, junctions and the other reparse points that name another path (not, for
    // instance, files kept in the cloud).
    if !m.file_type().is_symlink() {
        return Some(Seen::Plain);
    }
    Some(match std::fs::read_link(path) {
        Ok(t) if t.is_absolute() || netpath::host(&t).is_some() => Seen::Link(t),
        Ok(t) => Seen::Link(path.parent().unwrap_or(Path::new("")).join(t)),
        Err(_) => Seen::Unknown,
    })
}

/// `path` with `.` and `..` taken away by name, as Windows does before it follows links (not in
/// a verbatim `\\?\` path, which it takes as written).
fn lexical(path: &Path) -> PathBuf {
    if path.as_os_str().to_string_lossy().starts_with(r"\\?\") {
        return path.to_path_buf();
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            Component::ParentDir if out.has_root() => {}
            _ => out.push(c),
        }
    }
    out
}

fn denied(path: &Path, host: String) -> Error {
    if host.is_empty() {
        Error::new(ErrorKind::PermissionDenied, format!("{} may lead to a network host that is not allowed (security.allowedUNCHosts): through a link that leads there or that cannot be read", path.display()))
    } else {
        Error::new(ErrorKind::PermissionDenied, format!("the network host {host} is not among your allowed hosts (security.allowedUNCHosts)"))
    }
}

fn gate(path: &Path) -> Result<()> {
    blocked(path).map_or(Ok(()), |h| Err(denied(path, h)))
}

/// `std::fs::metadata`. The path itself is looked at as a link first: when it is not one, that
/// look is its metadata.
pub fn metadata(p: impl AsRef<Path>) -> Result<Metadata> {
    let p = p.as_ref();
    if let Some(h) = blocked_within(p, MAX_LINKS, false) {
        return Err(denied(p, h));
    }
    if !cfg!(windows) || !netpath::restricted() {
        return std::fs::metadata(p);
    }
    let m = std::fs::symlink_metadata(p)?;
    if !m.file_type().is_symlink() {
        return Ok(m);
    }
    gate(p)?;
    std::fs::metadata(p)
}

pub fn symlink_metadata(p: impl AsRef<Path>) -> Result<Metadata> {
    let p = p.as_ref();
    // The link itself is looked at, not followed: only its folders matter.
    if let Some(h) = blocked_within(p, MAX_LINKS, false) {
        return Err(denied(p, h));
    }
    std::fs::symlink_metadata(p)
}

pub fn exists(p: impl AsRef<Path>) -> bool {
    metadata(p).is_ok()
}

pub fn is_file(p: impl AsRef<Path>) -> bool {
    metadata(p).is_ok_and(|m| m.is_file())
}

pub fn is_dir(p: impl AsRef<Path>) -> bool {
    metadata(p).is_ok_and(|m| m.is_dir())
}

/// The bytes of a regular file (reading a FIFO named as a source would block); an error of kind
/// `InvalidInput` for anything else.
pub fn read_regular(p: impl AsRef<Path>) -> Result<Vec<u8>> {
    use std::io::Read;
    let p = p.as_ref();
    let m = metadata(p)?;
    if !m.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{} is not a regular file", p.display())));
    }
    // Larger than any source a project compiles: refused, not read into memory.
    if m.len() > MAX_REGULAR {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{} is larger than {} MB", p.display(), MAX_REGULAR >> 20)));
    }
    let mut b = Vec::with_capacity(m.len() as usize);
    std::fs::File::open(p)?.take(MAX_REGULAR).read_to_end(&mut b)?;
    Ok(b)
}

/// The largest file [`read_regular`] reads.
pub const MAX_REGULAR: u64 = 1 << 30;

pub fn read(p: impl AsRef<Path>) -> Result<Vec<u8>> {
    let p = p.as_ref();
    gate(p)?;
    std::fs::read(p)
}

pub fn open(p: impl AsRef<Path>) -> Result<std::fs::File> {
    let p = p.as_ref();
    gate(p)?;
    std::fs::File::open(p)
}

pub fn read_dir(p: impl AsRef<Path>) -> Result<std::fs::ReadDir> {
    let p = p.as_ref();
    gate(p)?;
    std::fs::read_dir(p)
}

pub fn canonicalize(p: impl AsRef<Path>) -> Result<PathBuf> {
    let p = p.as_ref();
    gate(p)?;
    std::fs::canonicalize(p)
}

/// Whether the folder of `path` usually ignores case: on Windows and macOS, and Windows
/// drives under WSL (`/mnt/c/...`). Two spellings there may be one file ([`same_file`] tells).
pub fn ignores_case(path: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        return true;
    }
    let b = path.as_os_str().as_encoded_bytes();
    cfg!(target_os = "linux") && b.len() >= 7 && b.starts_with(b"/mnt/") && b[5].is_ascii_alphabetic() && b[6] == b'/'
}

/// Whether two paths name one existing file: through links, `..`, or another case where case
/// is ignored.
pub fn same_file(a: impl AsRef<Path>, b: impl AsRef<Path>) -> bool {
    let (a, b) = (a.as_ref(), b.as_ref());
    #[cfg(unix)]
    if ignores_case(a) && a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase() {
        // A canonical path keeps the case it was written in on a drive under WSL: two
        // spellings are one file when the file is (not two names of a hard link, whose
        // folders may differ).
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (metadata(a), metadata(b)) {
            if x.ino() != 0 && x.dev() == y.dev() && x.ino() == y.ino() {
                return true;
            }
        }
    }
    matches!((canonicalize(a), canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn links_to_a_host_that_is_not_allowed_are_not_followed() {
        let _hosts = netpath::TEST_HOSTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("dvicode-fsx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/a.sv"), "module a; endmodule\n").unwrap();
        // A junction needs no privilege; to the folder here, then pretend it led to a host.
        let ok = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(dir.join("j")).arg(dir.join("real")).output().is_ok_and(|o| o.status.success());
        netpath::set_allowed_hosts(Some(vec!["build01".into()]));
        assert!(is_file(dir.join("real/a.sv")));
        if ok {
            assert!(is_file(dir.join("j/a.sv")), "a junction to a local folder is followed");
        }
        assert!(read(r"\\attacker\share\x.sv").is_err_and(|e| e.kind() == ErrorKind::PermissionDenied));
        assert!(!exists(r"\\.\UNC\attacker\share\x.sv"));
        assert!(read_dir(r"\\?\unc\attacker\share").is_err());
        assert_eq!(blocked_within(Path::new(r"\\attacker\share\x.sv"), MAX_LINKS, false).as_deref(), Some("attacker"));
        assert!(is_file(dir.join(r"j\..\real\a.sv")), "`..` by name, as Windows reads it");
        // Links as remembered (making symbolic links needs a privilege): the rest of the path
        // goes on from the target, through chains of links; one that cannot be read leads
        // anywhere.
        {
            let mut g = FOLDERS.write().unwrap();
            let m = g.get_or_insert_with(FxHashMap::default);
            m.insert(dir.join("to_host"), Seen::Link(PathBuf::from(r"\\attacker\share")));
            m.insert(dir.join("chain"), Seen::Link(dir.join("to_host")));
            m.insert(dir.join("to_real"), Seen::Link(dir.join("real")));
            m.insert(dir.join("unknown"), Seen::Unknown);
        }
        assert_eq!(blocked(dir.join(r"to_host\a.sv")).as_deref(), Some("attacker"));
        assert_eq!(blocked(dir.join(r"chain\a.sv")).as_deref(), Some("attacker"));
        assert_eq!(blocked(dir.join(r"unknown\a.sv")).as_deref(), Some(""));
        assert!(read(dir.join(r"chain\a.sv")).is_err_and(|e| e.kind() == ErrorKind::PermissionDenied));
        assert_eq!(blocked(dir.join(r"to_real\a.sv")), None);
        netpath::set_allowed_hosts(None);
        assert!(FOLDERS.read().unwrap().is_none(), "forgotten when the hosts change");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
