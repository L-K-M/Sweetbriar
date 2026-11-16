//! The macOS product version (spec/10's honest `User-Agent`): a
//! `/usr/bin/sw_vers` read (never a `PATH` lookup); `unknown`
//! elsewhere.

/// The host's macOS product version, or `"unknown"` where it cannot
/// be read (Linux, a missing `sw_vers`).
pub fn macos_version() -> String {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}
