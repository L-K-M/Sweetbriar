//! Default roots (spec/03 "On-disk layout"): where the store and the
//! disposable cache live when the caller doesn't choose. The path
//! logic is pure and compiled on every host — taking the OS shape, the
//! home directory and the XDG values as parameters — so both OS shapes
//! are unit-tested everywhere; only reading the environment differs
//! per host.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::location::{HOST_OS, HostOs};

/// The store's default data root: macOS
/// `~/Library/Application Support/Sweetbriar`; Linux
/// `$XDG_DATA_HOME/sweetbriar`, else `~/.local/share/sweetbriar`
/// (spec/03). Fails only when the home directory is unknown.
pub fn default_data_root() -> io::Result<PathBuf> {
    Ok(data_root_for(
        HOST_OS,
        &home()?,
        std::env::var_os("XDG_DATA_HOME"),
    ))
}

/// The disposable cache's default root: macOS
/// `~/Library/Caches/Sweetbriar`; Linux `$XDG_CACHE_HOME/sweetbriar`,
/// else `~/.cache/sweetbriar` (spec/03). Fails only when the home
/// directory is unknown.
pub fn default_cache_root() -> io::Result<PathBuf> {
    Ok(cache_root_for(
        HOST_OS,
        &home()?,
        std::env::var_os("XDG_CACHE_HOME"),
    ))
}

/// The user's home directory, or an error naming the gap.
fn home() -> io::Result<PathBuf> {
    std::env::home_dir().ok_or_else(|| io::Error::other("no home directory is known"))
}

/// The data root for an OS shape, a home and `XDG_DATA_HOME` (Linux
/// only).
fn data_root_for(os: HostOs, home: &Path, xdg_data: Option<OsString>) -> PathBuf {
    match os {
        HostOs::MacOs => home.join("Library/Application Support/Sweetbriar"),
        HostOs::Linux => linux_root(xdg_data, home, ".local/share"),
    }
}

/// The cache root for an OS shape, a home and `XDG_CACHE_HOME` (Linux
/// only).
fn cache_root_for(os: HostOs, home: &Path, xdg_cache: Option<OsString>) -> PathBuf {
    match os {
        HostOs::MacOs => home.join("Library/Caches/Sweetbriar"),
        HostOs::Linux => linux_root(xdg_cache, home, ".cache"),
    }
}

/// The Linux root for one XDG variable: only an absolute value counts
/// (spec/03); unset, empty or relative falls back to `home/<fallback>`.
/// The final `sweetbriar` segment joins either way.
fn linux_root(xdg: Option<OsString>, home: &Path, fallback: &str) -> PathBuf {
    let base = match xdg {
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        _ => home.join(fallback),
    };
    base.join("sweetbriar")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An injected home: the shape tests never read the environment.
    const HOME: &str = "/home/tester";

    #[test]
    fn macos_roots_are_under_library() {
        let ignored_xdg = Some(OsString::from("/custom/data"));
        assert_eq!(
            data_root_for(HostOs::MacOs, Path::new(HOME), ignored_xdg.clone()),
            Path::new("/home/tester/Library/Application Support/Sweetbriar")
        );
        assert_eq!(
            cache_root_for(HostOs::MacOs, Path::new(HOME), ignored_xdg),
            Path::new("/home/tester/Library/Caches/Sweetbriar")
        );
    }

    #[test]
    fn linux_roots_follow_absolute_xdg_values() {
        assert_eq!(
            data_root_for(HostOs::Linux, Path::new(HOME), Some("/custom/data".into())),
            Path::new("/custom/data/sweetbriar")
        );
        assert_eq!(
            cache_root_for(HostOs::Linux, Path::new(HOME), Some("/custom/cache".into())),
            Path::new("/custom/cache/sweetbriar")
        );
    }

    /// Unset, empty and relative XDG values don't count (spec/03): the
    /// fallback under the home directory is used.
    #[test]
    fn linux_roots_fall_back_when_xdg_is_missing_empty_or_relative() {
        for missing in [None, Some(String::new()), Some("relative/x".to_owned())] {
            let xdg = missing.map(OsString::from);
            assert_eq!(
                data_root_for(HostOs::Linux, Path::new(HOME), xdg.clone()),
                Path::new("/home/tester/.local/share/sweetbriar"),
                "XDG_DATA_HOME {xdg:?} must not count"
            );
            assert_eq!(
                cache_root_for(HostOs::Linux, Path::new(HOME), xdg),
                Path::new("/home/tester/.cache/sweetbriar")
            );
        }
    }

    /// The public functions apply this host's shape to the real home
    /// and XDG values. Nothing is created: the roots are only computed.
    #[test]
    fn public_roots_use_the_host_shape_and_real_environment() {
        let home = std::env::home_dir().expect("a home directory exists in tests");
        assert_eq!(
            default_data_root().unwrap(),
            data_root_for(HOST_OS, &home, std::env::var_os("XDG_DATA_HOME"))
        );
        assert_eq!(
            default_cache_root().unwrap(),
            cache_root_for(HOST_OS, &home, std::env::var_os("XDG_CACHE_HOME"))
        );
    }
}
