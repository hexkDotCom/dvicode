//! Network paths on Windows (`\\host\share\...`): reading one can make Windows sign in to
//! that host with the user's credentials, so a project (a build file, an `include`) must not
//! reach a host the user has not allowed. The editor tells which hosts are allowed (VS Code's
//! `security.allowedUNCHosts`, while `security.restrictUNCAccess` is on); without that, every
//! host is.

use std::path::{Component, Path, Prefix};
use std::sync::RwLock;

/// The hosts allowed, lowercase; `None`: every host.
static ALLOWED: RwLock<Option<Vec<String>>> = RwLock::new(None);

/// Held by tests that change the allowed hosts.
#[cfg(test)]
pub(crate) static TEST_HOSTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sets the hosts network paths may name (`None`: any).
pub fn set_allowed_hosts(hosts: Option<Vec<String>>) {
    *ALLOWED.write().unwrap() = hosts.map(|v| v.into_iter().map(|h| h.to_lowercase()).collect());
    crate::fsx::forget();
}

/// Whether some hosts are not allowed.
pub fn restricted() -> bool {
    ALLOWED.read().unwrap().is_some()
}

/// The host of a network path, `None` for any other path.
pub fn host(path: &Path) -> Option<String> {
    let mut parts = path.components();
    match parts.next()? {
        Component::Prefix(p) => match p.kind() {
            Prefix::UNC(host, _) | Prefix::VerbatimUNC(host, _) => Some(host.to_string_lossy().to_lowercase()),
            // `\\.\UNC\host\share` and `\\?\unc\host\share` (the verbatim form spelled in another
            // case): the host comes next.
            Prefix::DeviceNS(n) | Prefix::Verbatim(n) if n.eq_ignore_ascii_case("unc") => parts.find_map(|c| match c {
                Component::Normal(h) => Some(h.to_string_lossy().to_lowercase()),
                _ => None,
            }),
            _ => None,
        },
        _ => None,
    }
}

/// The host of `path` when it is a network path to a host that is not allowed (it must not be
/// read, listed or looked at).
pub fn blocked(path: &Path) -> Option<String> {
    let allowed = ALLOWED.read().unwrap();
    let list = allowed.as_ref()?;
    let Some(host) = host(path) else { return device_namespace(path) };
    // WSL's own file system is this machine.
    if host == "wsl.localhost" || host == "wsl$" {
        return None;
    }
    (!list.iter().any(|h| *h == host)).then_some(host)
}

/// The name of a namespace of devices other than drives that `path` is in: one may reach a
/// network host too (`\\?\GLOBALROOT\Device\Mup\host\share`), so none is read while hosts are
/// restricted.
fn device_namespace(path: &Path) -> Option<String> {
    let Some(Component::Prefix(p)) = path.components().next() else { return None };
    match p.kind() {
        Prefix::Verbatim(n) => Some(n.to_string_lossy().to_lowercase()),
        Prefix::DeviceNS(n) if !(n.len() == 2 && n.to_string_lossy().ends_with(':')) => Some(n.to_string_lossy().to_lowercase()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn network_paths_reach_only_the_hosts_allowed() {
        let _hosts = TEST_HOSTS.lock().unwrap_or_else(|e| e.into_inner());
        let p = Path::new(r"\\build01\share\rtl\top.sv");
        assert_eq!(host(p).as_deref(), Some("build01"));
        assert_eq!(host(Path::new(r"\\?\UNC\Build01\share\x.sv")).as_deref(), Some("build01"));
        assert_eq!(host(Path::new(r"\\.\UNC\Build01\share\x.sv")).as_deref(), Some("build01"));
        assert_eq!(host(Path::new(r"\\?\unc\Build01\share\x.sv")).as_deref(), Some("build01"));
        assert_eq!(host(Path::new(r"\\.\C:\work\x.sv")), None);
        assert_eq!(host(Path::new(r"C:\work\x.sv")), None);
        set_allowed_hosts(None);
        assert_eq!(blocked(p), None);
        set_allowed_hosts(Some(vec!["BUILD01".into()]));
        assert_eq!(blocked(p), None);
        assert_eq!(blocked(Path::new(r"\\attacker\share\x.svh")).as_deref(), Some("attacker"));
        assert_eq!(blocked(Path::new(r"\\wsl.localhost\Ubuntu\home\x.sv")), None);
        assert_eq!(blocked(Path::new(r"C:\work\x.sv")), None);
        assert_eq!(blocked(Path::new(r"\\?\C:\work\x.sv")), None);
        assert_eq!(blocked(Path::new(r"\\.\C:\work\x.sv")), None);
        assert_eq!(blocked(Path::new(r"\\?\GLOBALROOT\Device\Mup\attacker\share\x.sv")).as_deref(), Some("globalroot"));
        assert_eq!(blocked(Path::new(r"\\.\pipe\x")).as_deref(), Some("pipe"));
        // A build file naming a host that is not allowed: a problem, and nothing read there.
        let dir = std::env::temp_dir().join(format!("dvicode-netpath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let text = "\\\\attacker\\share\\x.sv\n+incdir+\\\\attacker\\inc\n-f \\\\attacker\\share\\more.f\n";
        let cfg = crate::parse_build(&dir, &dir.join("x.build"), text, &Default::default());
        assert!(cfg.invocations.iter().all(|i| i.files.is_empty() && i.incdirs.is_empty()), "{:?}", cfg.invocations);
        assert_eq!(cfg.problems.iter().filter(|p| p.message.contains("network host attacker")).count(), 3, "{:?}", cfg.problems);
        set_allowed_hosts(None);
    }
}
