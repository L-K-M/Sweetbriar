//! The macOS product version (spec/10's honest `User-Agent`): a
//! `sw_vers` read; `unknown` elsewhere.

/// The host's macOS product version, or `"unknown"` where it cannot
/// be read (Linux, a missing `sw_vers`).
pub fn macos_version() -> String {
    std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}
