//! Where the access keys are kept: one small text file per role in the
//! user's own settings directory, readable only by that user where the
//! system supports it.

use crate::auth;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// `%APPDATA%\oink-remote` on Windows, `~/Library/Application Support/oink-remote`
/// on macOS, `$XDG_CONFIG_HOME/oink-remote` or `~/.config/oink-remote` elsewhere.
pub fn directory() -> io::Result<PathBuf> {
    let variable = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        variable("APPDATA")
    } else if cfg!(target_os = "macos") {
        variable("HOME").map(|home| home.join("Library/Application Support"))
    } else {
        variable("XDG_CONFIG_HOME").or_else(|| variable("HOME").map(|home| home.join(".config")))
    };
    base.map(|base| base.join("oink-remote"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no settings directory for this user"))
}

fn write_private(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    io::Write::write_all(&mut options.open(path)?, text.as_bytes())
}

/// The host's access code, created on first use. `renew` replaces it.
pub fn host_code(directory: &Path, renew: bool) -> io::Result<String> {
    let path = directory.join("host-key.txt");
    if !renew && let Some(code) = fs::read_to_string(&path).ok().as_deref().and_then(auth::normalize_code) {
        return Ok(auth::display_code(&code));
    }
    let code = auth::generate_code();
    write_private(&path, &format!("{code}\n"))?;
    Ok(code)
}

fn saved_hosts(directory: &Path) -> Vec<(String, String)> {
    let text = fs::read_to_string(directory.join("saved-hosts.txt")).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(host, code)| (host.to_owned(), code.trim().to_owned()))
        .collect()
}

/// The code saved for this host name or address on the viewer.
pub fn saved_code(directory: &Path, host: &str) -> Option<String> {
    saved_hosts(directory)
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(host))
        .map(|(_, code)| code)
}

/// Remember the code for this host after it was accepted.
pub fn save_code(directory: &Path, host: &str, code: &str) -> io::Result<()> {
    let mut hosts = saved_hosts(directory);
    hosts.retain(|(name, _)| !name.eq_ignore_ascii_case(host));
    hosts.push((host.to_owned(), code.to_owned()));
    let text: String = hosts.iter().map(|(name, code)| format!("{name} {code}\n")).collect();
    write_private(&directory.join("saved-hosts.txt"), &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("oink-remote-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn host_code_is_kept_until_renewed() {
        let directory = scratch("host");
        let first = host_code(&directory, false).unwrap();
        assert_eq!(host_code(&directory, false).unwrap(), first);
        assert_ne!(host_code(&directory, true).unwrap(), first);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn viewer_remembers_one_code_per_host() {
        let directory = scratch("viewer");
        assert_eq!(saved_code(&directory, "pc"), None);
        save_code(&directory, "pc", "AAAAA-BBBBB-CCCCC-DDDDD").unwrap();
        save_code(&directory, "100.64.0.9", "EEEEE-FFFFF-GGGGG-HHHHH").unwrap();
        save_code(&directory, "PC", "JJJJJ-KKKKK-LLLLL-MMMMM").unwrap();
        assert_eq!(saved_code(&directory, "pc").as_deref(), Some("JJJJJ-KKKKK-LLLLL-MMMMM"));
        assert_eq!(
            saved_code(&directory, "100.64.0.9").as_deref(),
            Some("EEEEE-FFFFF-GGGGG-HHHHH")
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
