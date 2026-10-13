//! `Fs::location` classification (spec/03 "The data root lives on a
//! local filesystem"): synced folders are detected by path prefix
//! under the home directory, network mounts by filesystem type. The
//! matching is pure and compiled on every host so both OS tables are
//! unit-tested everywhere; only the `statfs` call differs per OS.

use std::ffi::{CString, OsString};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sb_types::fs::Location;

/// The host OS whose rules classify a path; `synced_service` takes it
/// as a parameter so the other OS's table still compiles and is tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostOs {
    Linux,
    MacOs,
}

const HOST_OS: HostOs = if cfg!(target_os = "macos") {
    HostOs::MacOs
} else {
    HostOs::Linux
};

/// The `Location` of `path`, or of its nearest existing ancestor when
/// `path` does not exist: synced by home-relative prefix, then network
/// by filesystem type, else local.
pub(super) fn classify(path: &Path) -> io::Result<Location> {
    let (existing, tail) = nearest_existing(path)?;
    let mut probe = existing.canonicalize()?;
    for missing in tail {
        probe.push(missing);
    }

    if let Some(service) = synced_service(&probe, &canonical_home()?, HOST_OS) {
        return Ok(Location::Synced { service });
    }
    if let Some(filesystem) = statfs_network_name(&statfs(&existing)?) {
        return Ok(Location::Network { filesystem });
    }
    Ok(Location::Local)
}

/// The deepest existing ancestor of `path` (itself when it exists) and
/// the components missing below it, in path order.
fn nearest_existing(path: &Path) -> io::Result<(PathBuf, Vec<OsString>)> {
    let mut missing = Vec::new();
    for ancestor in path.ancestors() {
        match ancestor.try_exists() {
            Ok(true) => {
                missing.reverse();
                return Ok((ancestor.to_path_buf(), missing));
            }
            Ok(false) => {
                if let Some(name) = ancestor.file_name() {
                    missing.push(name.to_os_string());
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} has no existing ancestor", path.display()),
    ))
}

fn canonical_home() -> io::Result<PathBuf> {
    std::env::home_dir()
        .ok_or_else(|| io::Error::other("no home directory"))?
        .canonicalize()
}

/// The sync service a path under `home` belongs to, matching
/// component-wise: `~/Dropboxes` is not Dropbox, `~/OneDrive - Acme`
/// is OneDrive.
fn synced_service(path: &Path, home: &Path, os: HostOs) -> Option<String> {
    let mut components = path.strip_prefix(home).ok()?.components();
    match (os, components.next()?.as_os_str().to_str()?) {
        (HostOs::MacOs, "Library") => synced_library_folder(&mut components),
        (_, "Dropbox") => Some("Dropbox".to_owned()),
        (_, first) if first.starts_with("OneDrive") => Some("OneDrive".to_owned()),
        _ => None,
    }
}

/// The sync service owning a folder inside `~/Library` (macOS), or
/// None when `~/Library/<name>` is local.
fn synced_library_folder(components: &mut std::path::Components<'_>) -> Option<String> {
    let service = match components.next()?.as_os_str().to_str()? {
        // iCloud Drive's container folder.
        "Mobile Documents" => "iCloud Drive",
        // One child folder per account, named `<Provider>-<account>`
        // (`OneDrive-Personal`, `GoogleDrive-o@example.com`). An
        // unknown provider (or CloudStorage itself) still counts as
        // synced.
        "CloudStorage" => {
            let folder = components.next().and_then(|c| c.as_os_str().to_str());
            match folder.and_then(|f| f.split('-').next()) {
                Some("Dropbox") => "Dropbox",
                Some("OneDrive") => "OneDrive",
                Some("GoogleDrive") => "Google Drive",
                Some("Box") => "Box",
                _ => "a cloud provider",
            }
        }
        _ => return None,
    };
    Some(service.to_owned())
}

/// `MNT_LOCAL` (sys/mount.h): the mount is a local filesystem. Copied
/// here so the flag test compiles and is unit-tested on every host.
#[cfg_attr(not(target_os = "macos"), cfg(test))]
const MACOS_MNT_LOCAL: u32 = 0x0000_1000;

/// Whether a macOS `statfs.f_flags` value marks a remote mount.
#[cfg_attr(not(target_os = "macos"), cfg(test))]
fn macos_mount_is_network(f_flags: u32) -> bool {
    f_flags & MACOS_MNT_LOCAL == 0
}

/// Kernel network filesystems by `statfs.f_type` magic (linux/magic.h).
/// FUSE is absent on purpose: it also serves local filesystems (e.g.
/// ntfs-3g, gocryptfs), so a FUSE mount stays local — spec/03
/// classifies by kernel filesystem type only.
#[cfg_attr(not(target_os = "linux"), cfg(test))]
fn linux_network_fs_name(f_type: i64) -> Option<&'static str> {
    const NFS_SUPER_MAGIC: i64 = 0x6969;
    const SMB_SUPER_MAGIC: i64 = 0x517B;
    const SMB2_SUPER_MAGIC: i64 = 0xFE53_4D42;
    const CIFS_SUPER_MAGIC: i64 = 0xFF53_4D42;
    const CODA_SUPER_MAGIC: i64 = 0x7375_7245;
    const AFS_SUPER_MAGIC: i64 = 0x5346_414F;
    const V9FS_MAGIC: i64 = 0x0102_1997;
    const CEPH_SUPER_MAGIC: i64 = 0x00C3_6400;
    match f_type {
        NFS_SUPER_MAGIC => Some("NFS"),
        SMB_SUPER_MAGIC => Some("SMB"),
        SMB2_SUPER_MAGIC => Some("SMB2"),
        CIFS_SUPER_MAGIC => Some("CIFS"),
        CODA_SUPER_MAGIC => Some("Coda"),
        AFS_SUPER_MAGIC => Some("AFS"),
        V9FS_MAGIC => Some("9P"),
        CEPH_SUPER_MAGIC => Some("Ceph"),
        _ => None,
    }
}

fn statfs(path: &Path) -> io::Result<libc::statfs> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is a NUL-terminated byte string alive for the
    // call, and `stat` is valid, exclusively owned output storage the
    // kernel fills on success.
    if unsafe { libc::statfs(c_path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statfs returned 0, so `stat` is initialized.
    Ok(unsafe { stat.assume_init() })
}

/// The network filesystem a `statfs` describes, or None when local.
#[cfg(target_os = "linux")]
fn statfs_network_name(stat: &libc::statfs) -> Option<String> {
    linux_network_fs_name(stat.f_type).map(str::to_owned)
}

/// The network filesystem a `statfs` describes, or None when local,
/// named by `f_fstypename` (`smbfs`, `osxfuse` and friends).
#[cfg(target_os = "macos")]
fn statfs_network_name(stat: &libc::statfs) -> Option<String> {
    if !macos_mount_is_network(stat.f_flags) {
        return None;
    }
    let end = stat
        .f_fstypename
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(stat.f_fstypename.len());
    let name: Vec<u8> = stat.f_fstypename[..end].iter().map(|&c| c as u8).collect();
    match String::from_utf8_lossy(&name) {
        s if s.is_empty() => Some("unknown".to_owned()),
        s => Some(s.into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_folders_under_home_macos() {
        let home = Path::new("/Users/o");
        let at = |p: &str| synced_service(Path::new(p), home, HostOs::MacOs);
        assert_eq!(
            at("/Users/o/Library/Mobile Documents/com~apple~CloudDocs/x").as_deref(),
            Some("iCloud Drive")
        );
        assert_eq!(
            at("/Users/o/Library/CloudStorage/Dropbox/x").as_deref(),
            Some("Dropbox")
        );
        assert_eq!(
            at("/Users/o/Library/CloudStorage/GoogleDrive-o@example.com/x").as_deref(),
            Some("Google Drive")
        );
        assert_eq!(
            at("/Users/o/Library/CloudStorage/OneDrive-Personal/x").as_deref(),
            Some("OneDrive")
        );
        assert_eq!(
            at("/Users/o/Library/CloudStorage/Nextcloud/x").as_deref(),
            Some("a cloud provider")
        );
        assert_eq!(at("/Users/o/Dropbox/mail").as_deref(), Some("Dropbox"));
        assert_eq!(
            at("/Users/o/OneDrive - Acme/mail").as_deref(),
            Some("OneDrive")
        );
        assert_eq!(at("/Users/o/OneDrive").as_deref(), Some("OneDrive"));
    }

    #[test]
    fn synced_folders_under_home_linux() {
        let home = Path::new("/home/o");
        let at = |p: &str| synced_service(Path::new(p), home, HostOs::Linux);
        assert_eq!(at("/home/o/Dropbox/mail").as_deref(), Some("Dropbox"));
        assert_eq!(
            at("/home/o/OneDrive - Acme/mail").as_deref(),
            Some("OneDrive")
        );
        // macOS's special folders are ordinary directories on Linux.
        assert_eq!(at("/home/o/Library/Mobile Documents/x"), None);
        assert_eq!(at("/home/o/Library/CloudStorage/Dropbox/x"), None);
    }

    #[test]
    fn synced_folders_match_components_not_substrings() {
        let home = Path::new("/home/o");
        let at = |p: &str| synced_service(Path::new(p), home, HostOs::Linux);
        assert_eq!(at("/home/o/Dropboxes/x"), None);
        assert_eq!(at("/home/o/dropbox/x"), None);
        assert_eq!(at("/other/Dropbox/x"), None);
        assert_eq!(at("/home/o/Mail/x"), None);
        // The home directory itself is not a synced folder.
        assert_eq!(at("/home/o"), None);
    }

    #[test]
    fn network_filesystems_by_magic() {
        for (magic, name) in [
            (0x6969, "NFS"),
            (0x517B, "SMB"),
            (0xFE534D42, "SMB2"),
            (0xFF534D42, "CIFS"),
            (0x73757245, "Coda"),
            (0x5346414F, "AFS"),
            (0x01021997, "9P"),
            (0x00C36400, "Ceph"),
        ] {
            assert_eq!(linux_network_fs_name(magic), Some(name));
        }
        // Local filesystems: ext4, tmpfs, btrfs, and FUSE (which also
        // mounts local filesystems).
        for magic in [0xEF53, 0x01021994, 0x9123683E, 0x65735546] {
            assert_eq!(linux_network_fs_name(magic), None);
        }
    }

    #[test]
    fn network_mounts_by_macos_flags() {
        assert!(!macos_mount_is_network(MACOS_MNT_LOCAL));
        assert!(macos_mount_is_network(0));
        assert!(macos_mount_is_network(0x0000_4000)); // MNT_DONTBROWSE alone
    }
}
