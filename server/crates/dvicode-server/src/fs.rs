//! Remote file system requests (`dvicode/fs/*`) backing the client's `dvicode://` scheme.
//!
//! Types and errors follow VS Code's `FileSystemProvider`: type 1 = file, 2 = directory,
//! 64 = symbolic link (combined with the target's type); errors carry a `code` of
//! `FileNotFound`, `FileExists`, `NoPermissions`, `FileIsADirectory`, `FileNotADirectory` or
//! `Unavailable`.

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub struct FsError {
    pub code: &'static str,
    pub message: String,
}

fn err(e: std::io::Error, path: &Path) -> FsError {
    use std::io::ErrorKind::*;
    let code = match e.kind() {
        NotFound => "FileNotFound",
        AlreadyExists => "FileExists",
        PermissionDenied => "NoPermissions",
        _ => "Unavailable",
    };
    FsError { code, message: format!("{}: {e}", path.display()) }
}

#[derive(Deserialize)]
struct PathParam {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteParam {
    path: String,
    content: String,
    #[serde(default = "yes")]
    create: bool,
    #[serde(default = "yes")]
    overwrite: bool,
}

#[derive(Deserialize)]
struct DeleteParam {
    path: String,
    #[serde(default)]
    recursive: bool,
}

#[derive(Deserialize)]
struct RenameParam {
    from: String,
    to: String,
    #[serde(default)]
    overwrite: bool,
}

fn yes() -> bool {
    true
}

fn ms(t: std::io::Result<std::time::SystemTime>) -> u64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn file_type(p: &Path) -> u32 {
    let link = std::fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    let base = match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => 2,
        Ok(m) if m.is_file() => 1,
        _ => 0,
    };
    base | if link { 64 } else { 0 }
}

/// Expands `~` and makes the path absolute.
fn path_of(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(s)
}

/// Handles one `dvicode/fs/<op>` request. Returns `Ok(result)` or an error with a code.
pub fn handle(op: &str, params: Value) -> Result<Value, FsError> {
    let bad = |e: serde_json::Error| FsError { code: "Unavailable", message: format!("bad parameters: {e}") };
    match op {
        "home" => Ok(json!({ "path": std::env::var("HOME").unwrap_or_else(|_| "/".into()), "host": hostname() })),
        "stat" => {
            let p: PathParam = serde_json::from_value(params).map_err(bad)?;
            let path = path_of(&p.path);
            let m = std::fs::metadata(&path).map_err(|e| err(e, &path))?;
            Ok(json!({
                "type": file_type(&path),
                "size": m.len(),
                "mtime": ms(m.modified()),
                "ctime": ms(m.created().or_else(|_| m.modified())),
                "readonly": m.permissions().readonly(),
            }))
        }
        "readDirectory" => {
            let p: PathParam = serde_json::from_value(params).map_err(bad)?;
            let path = path_of(&p.path);
            let rd = std::fs::read_dir(&path).map_err(|e| err(e, &path))?;
            let mut v: Vec<Value> = rd
                .flatten()
                .map(|e| json!([e.file_name().to_string_lossy(), file_type(&e.path())]))
                .collect();
            v.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
            Ok(Value::Array(v))
        }
        "readFile" => {
            let p: PathParam = serde_json::from_value(params).map_err(bad)?;
            let path = path_of(&p.path);
            if path.is_dir() {
                return Err(FsError { code: "FileIsADirectory", message: path.display().to_string() });
            }
            let b = std::fs::read(&path).map_err(|e| err(e, &path))?;
            Ok(json!({ "content": base64::engine::general_purpose::STANDARD.encode(b) }))
        }
        "writeFile" => {
            let p: WriteParam = serde_json::from_value(params).map_err(bad)?;
            let path = path_of(&p.path);
            let exists = path.exists();
            if exists && !p.overwrite {
                return Err(FsError { code: "FileExists", message: path.display().to_string() });
            }
            if !exists && !p.create {
                return Err(FsError { code: "FileNotFound", message: path.display().to_string() });
            }
            let data = base64::engine::general_purpose::STANDARD
                .decode(p.content.as_bytes())
                .map_err(|e| FsError { code: "Unavailable", message: format!("bad content: {e}") })?;
            std::fs::write(&path, data).map_err(|e| err(e, &path))?;
            Ok(Value::Null)
        }
        "createDirectory" => {
            let p: PathParam = serde_json::from_value(params).map_err(bad)?;
            let path = path_of(&p.path);
            std::fs::create_dir(&path).map_err(|e| err(e, &path))?;
            Ok(Value::Null)
        }
        "delete" => {
            let p: DeleteParam = serde_json::from_value(params).map_err(bad)?;
            let path = path_of(&p.path);
            let r = if path.is_dir() {
                if p.recursive {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_dir(&path)
                }
            } else {
                std::fs::remove_file(&path)
            };
            r.map_err(|e| err(e, &path))?;
            Ok(Value::Null)
        }
        "rename" => {
            let p: RenameParam = serde_json::from_value(params).map_err(bad)?;
            let (from, to) = (path_of(&p.from), path_of(&p.to));
            if to.exists() && !p.overwrite {
                return Err(FsError { code: "FileExists", message: to.display().to_string() });
            }
            std::fs::rename(&from, &to).map_err(|e| err(e, &from))?;
            Ok(Value::Null)
        }
        _ => Err(FsError { code: "Unavailable", message: format!("unknown file system operation {op}") }),
    }
}

pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "localhost".into())
}
