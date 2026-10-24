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

/// Folders known to lead to no host that is not allowed, through no link on the way (`true`), or
/// to lead to one (`false`). Cleared when the allowed hosts change.
static FOLDERS: RwLock<Option<FxHashMap<PathBuf, bool>>> = RwLock::new(None);

/// Links followed from one path at most (a longer chain is taken as leading somewhere blocked).
const MAX_LINKS: usize = 8;

/// Forgets what is known of folders (the allowed hosts changed).
pub fn forget() {
    *FOLDERS.write().unwrap() = None;
}

/// The host a path leads to that is not allowed: written in it, or the target of a link on the
/// way (an empty name when only a link leads there).
pub fn blocked(path: impl AsRef<Path>) -> Option<String> {
    blocked_within(path.as_ref(), MAX_LINKS, true)
}

/// Whether reaching `path` may sign in to a network host that is not allowed. Links are looked
/// at only on Windows, where a link to `\\host\share` is followed by the system; `leaf`: the
/// path itself too, not only its folders.
fn blocked_within(path: &Path, links: usize, leaf: bool) -> Option<String> {
    if let Some(h) = netpath::blocked(path) {
        return Some(h);
    }
    if !cfg!(windows) || !netpath::restricted() {
        return None;
    }
    let n = path.components().count();
    let mut cur = PathBuf::new();
    for (i, c) in path.components().enumerate() {
        cur.push(c);
        if matches!(c, Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir) {
            continue;
        }
        let last = i + 1 == n;
        if last && !leaf {
            break;
        }
        if !last {
            if let Some(known) = FOLDERS.read().unwrap().as_ref().and_then(|m| m.get(&cur).copied()) {
                if !known {
                    return Some(String::new());
                }
                continue;
            }
        }
        let bad = link_target(&cur).and_then(|t| if links == 0 { Some(String::new()) } else { blocked_within(&t, links - 1, true) });
        if !last {
            FOLDERS.write().unwrap().get_or_insert_with(FxHashMap::default).insert(cur.clone(), bad.is_none());
        }
        if bad.is_some() {
            return bad;
        }
    }
    None
}

/// Where `path` leads when it is a link (a symbolic link or a junction), as an absolute path.
fn link_target(path: &Path) -> Option<PathBuf> {
    let m = std::fs::symlink_metadata(path).ok()?;
    is_link(&m).then(|| std::fs::read_link(path).ok()).flatten().map(|t| if t.is_absolute() || netpath::host(&t).is_some() { t } else { path.parent().unwrap_or(Path::new("")).join(t) })
}

fn is_link(m: &Metadata) -> bool {
    m.file_type().is_symlink() || is_reparse_point(m)
}

#[cfg(windows)]
fn is_reparse_point(m: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_REPARSE_POINT: junctions, and others (cloud files) that lead nowhere else.
    m.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &Metadata) -> bool {
    false
}

fn denied(path: &Path, host: String) -> Error {
    if host.is_empty() {
        Error::new(ErrorKind::PermissionDenied, format!("{} leads to a network host that is not allowed (security.allowedUNCHosts)", path.display()))
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
    if !is_link(&m) {
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
    let p = p.as_ref();
    if !metadata(p)?.is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{} is not a regular file", p.display())));
    }
    std::fs::read(p)
}

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

#[cfg(test)]
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
        // A remembered folder that leads to a host is blocked without being looked at again.
        FOLDERS.write().unwrap().get_or_insert_with(FxHashMap::default).insert(dir.join("j"), false);
        assert!(!is_file(dir.join("j/a.sv")));
        assert!(read(dir.join("j/a.sv")).is_err());
        netpath::set_allowed_hosts(None);
        assert!(FOLDERS.read().unwrap().is_none(), "forgotten when the hosts change");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
