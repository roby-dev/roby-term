use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/roby-dev/roby-term/releases/latest";
pub const RELEASES_PAGE_URL: &str = "https://github.com/roby-dev/roby-term/releases/latest";

const TRUSTED_RELEASE_TAG_PREFIX: &str = "https://github.com/roby-dev/roby-term/releases/tag/";
const TRUSTED_DOWNLOAD_PREFIX: &str = "https://github.com/roby-dev/roby-term/releases/download/";
const MIN_BINARY_BYTES: u64 = 200_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: String,
    pub html_url: String,
    pub download_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading(String),
    ReadyToRestart { version: String, exe_path: PathBuf },
    Error { message: String, from_install: bool },
}

impl Default for UpdateStatus {
    fn default() -> Self {
        Self::Idle
    }
}

impl UpdateStatus {
    pub fn shows_header_pill(&self) -> bool {
        matches!(
            self,
            Self::Available(_)
                | Self::Downloading(_)
                | Self::ReadyToRestart { .. }
                | Self::Error {
                    from_install: true,
                    ..
                }
        )
    }
}

/// Parses a semantic version like `"v0.7.0"` or `"0.8.1-beta"` into `(major, minor, patch)`.
pub fn parse_semver(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    let core = trimmed.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    Some((major, minor, patch))
}

/// Returns `true` when `latest` is strictly newer than `current`.
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(cur), Some(lat)) => lat > cur,
        _ => false,
    }
}

/// Validates a GitHub release tag so it cannot contain path traversal, separators, or URL syntax.
pub fn is_safe_release_tag(tag: &str) -> bool {
    !tag.is_empty()
        && !tag.contains("..")
        && tag.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Validates that a download URL matches the exact official GitHub release asset path shape:
/// `https://github.com/roby-dev/roby-term/releases/download/<safe_tag>/<asset_name>`.
pub fn is_trusted_download_url(url: &str, asset_name: &str) -> bool {
    let Some(rest) = url.strip_prefix(TRUSTED_DOWNLOAD_PREFIX) else {
        return false;
    };
    let mut parts = rest.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(tag_seg), Some(file_seg), None) => {
            is_safe_release_tag(tag_seg) && file_seg == asset_name
        }
        _ => false,
    }
}

fn is_trusted_release_tag_url(url: &str) -> bool {
    url.strip_prefix(TRUSTED_RELEASE_TAG_PREFIX)
        .is_some_and(is_safe_release_tag)
}

pub fn platform_asset_name() -> Option<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("roby-term-windows-x64.exe")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("roby-term-macos-arm64")
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        None
    }
}

/// Extracts a JSON string value for `"key": "value"` without external JSON dependencies.
/// Skips occurrences where `"key"` is not followed by `:` and a JSON string value.
fn extract_json_string_after(haystack: &str, key: &str) -> Option<(String, usize)> {
    let needle = format!("\"{key}\"");
    let mut cursor = 0;
    while cursor < haystack.len() {
        let rel_pos = haystack[cursor..].find(&needle)?;
        let key_end = cursor + rel_pos + needle.len();
        let rest = &haystack[key_end..];
        let after_key = rest.trim_start();
        if !after_key.starts_with(':') {
            cursor = key_end;
            continue;
        }
        let colon_offset = key_end + (rest.len() - after_key.len()) + 1;
        let after_colon_raw = &haystack[colon_offset..];
        let after_colon = after_colon_raw.trim_start();
        if !after_colon.starts_with('"') {
            cursor = colon_offset;
            continue;
        }
        let quote_start = colon_offset + (after_colon_raw.len() - after_colon.len());
        let mut out = String::new();
        let mut chars = haystack[quote_start + 1..].char_indices();
        while let Some((idx, ch)) = chars.next() {
            if ch == '\\' {
                if let Some((_, next)) = chars.next() {
                    out.push(match next {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                }
                continue;
            }
            if ch == '"' {
                let consumed = quote_start + 1 + idx + 1;
                return Some((out, consumed));
            }
            out.push(ch);
        }
        return None;
    }
    None
}

/// Parses GitHub `/releases/latest` JSON payload for tag, release page URL, and platform asset URL.
pub fn parse_github_release_json(json: &str, asset_name: &str) -> Option<ReleaseInfo> {
    let (tag, _) = extract_json_string_after(json, "tag_name")?;
    if !is_safe_release_tag(&tag) {
        return None;
    }
    let version = tag.trim().trim_start_matches(['v', 'V']).to_string();
    parse_semver(&version)?;

    // Prefer the official release html_url, falling back to canonical URL.
    let mut html_url = format!("{TRUSTED_RELEASE_TAG_PREFIX}{tag}");
    let mut scan = json;
    while let Some((val, consumed)) = extract_json_string_after(scan, "html_url") {
        if is_trusted_release_tag_url(&val) {
            html_url = val;
            break;
        }
        scan = &scan[consumed..];
    }

    // Search all browser_download_url entries for the current platform's trusted asset.
    let mut download_url = format!("{TRUSTED_DOWNLOAD_PREFIX}{tag}/{asset_name}");
    let mut scan = json;
    while let Some((val, consumed)) = extract_json_string_after(scan, "browser_download_url") {
        if is_trusted_download_url(&val, asset_name) {
            download_url = val;
            break;
        }
        scan = &scan[consumed..];
    }

    Some(ReleaseInfo {
        tag,
        version,
        html_url,
        download_url,
    })
}

fn hidden_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn curl_program() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "curl.exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        "curl"
    }
}

/// Queries GitHub Releases API in a blocking call (intended for a background thread).
/// Returns `Ok(Some(info))` if a newer version exists, `Ok(None)` if up to date.
pub fn check_latest_release() -> Result<Option<ReleaseInfo>, String> {
    let asset_name = platform_asset_name()
        .ok_or_else(|| "auto-update is not supported on this architecture".to_string())?;

    let output = hidden_command(curl_program())
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--globoff",
            "--max-time",
            "8",
            "-H",
            "User-Agent: roby-term",
            "-H",
            "Accept: application/vnd.github+json",
            "--",
            RELEASES_API_URL,
        ])
        .output()
        .map_err(|e| format!("failed to run curl: {e}"))?;

    if !output.status.success() {
        return Err(format!("update check HTTP error ({})", output.status));
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let info = parse_github_release_json(&body, asset_name)
        .ok_or_else(|| "could not parse GitHub release metadata".to_string())?;

    if is_newer_version(CURRENT_VERSION, &info.version) {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

/// Verifies magic bytes of a downloaded executable to guard against HTML error pages.
pub fn validate_binary_header(bytes: &[u8]) -> bool {
    #[cfg(target_os = "windows")]
    {
        bytes.len() >= 2 && bytes[0] == b'M' && bytes[1] == b'Z'
    }
    #[cfg(target_os = "macos")]
    {
        if bytes.len() < 4 {
            return false;
        }
        let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
        matches!(
            magic,
            [0xCF, 0xFA, 0xED, 0xFE]
                | [0xFE, 0xED, 0xFA, 0xCF]
                | [0xCA, 0xFE, 0xBA, 0xBE]
                | [0xBE, 0xBA, 0xFE, 0xCA]
        )
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        bytes.len() >= 4 && &bytes[0..4] == b"\x7fELF"
    }
}

fn sibling_with_suffix(exe: &Path, suffix: &str) -> PathBuf {
    let mut os = exe.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// Removes a leftover `<exe>.old` binary from a previous in-place update on Windows.
pub fn cleanup_old_binary() {
    if let Ok(exe) = std::env::current_exe() {
        let old = sibling_with_suffix(&exe, ".old");
        if old.exists() {
            let _ = fs::remove_file(old);
        }
        let staging = sibling_with_suffix(&exe, ".new");
        if staging.exists() {
            let _ = fs::remove_file(staging);
        }
    }
}

/// Reads only the first 8 bytes of a file to verify executable magic bytes with minimal RAM.
pub fn read_file_header(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("cannot verify download: {e}"))?;
    let mut buf = [0u8; 8];
    let n = file
        .read(&mut buf)
        .map_err(|e| format!("cannot verify download: {e}"))?;
    Ok(buf[..n].to_vec())
}

struct StagingCleanupGuard(Option<PathBuf>);

impl StagingCleanupGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for StagingCleanupGuard {
    fn drop(&mut self) {
        if let Some(ref path) = self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// Downloads the release asset and replaces the current executable safely in place.
/// Returns `(version, target_exe_path)` captured before replacing the running binary.
pub fn download_and_install_update(info: &ReleaseInfo) -> Result<(String, PathBuf), String> {
    let asset_name = platform_asset_name()
        .ok_or_else(|| "auto-update is not supported on this architecture".to_string())?;
    if !is_trusted_download_url(&info.download_url, asset_name) {
        return Err("untrusted update download URL".to_string());
    }
    let current_exe =
        std::env::current_exe().map_err(|e| format!("cannot locate current binary: {e}"))?;
    let staging = sibling_with_suffix(&current_exe, ".new");
    let _ = fs::remove_file(&staging);
    let mut staging_guard = StagingCleanupGuard::new(staging.clone());

    let staging_str = staging
        .to_str()
        .ok_or_else(|| "invalid executable path".to_string())?;

    let status = hidden_command(curl_program())
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--globoff",
            "--max-time",
            "90",
            "-H",
            "User-Agent: roby-term",
            "-o",
            staging_str,
            "--",
            &info.download_url,
        ])
        .status()
        .map_err(|e| format!("download failed to start: {e}"))?;

    if !status.success() {
        return Err(format!("download failed ({status})"));
    }

    let meta = fs::metadata(&staging).map_err(|e| format!("downloaded file missing: {e}"))?;
    if meta.len() < MIN_BINARY_BYTES {
        return Err("downloaded file is unexpectedly small".to_string());
    }

    let header = read_file_header(&staging)?;
    if !validate_binary_header(&header) {
        return Err("downloaded file is not a valid executable".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        let backup = sibling_with_suffix(&current_exe, ".old");
        let _ = fs::remove_file(&backup);
        fs::rename(&current_exe, &backup)
            .map_err(|e| format!("cannot replace running binary (permissions?): {e}"))?;
        if let Err(e) = fs::rename(&staging, &current_exe) {
            let _ = fs::rename(&backup, &current_exe);
            return Err(format!("failed to finalize update: {e}"));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&staging, perms)
            .map_err(|e| format!("failed to chmod new binary: {e}"))?;
        fs::rename(&staging, &current_exe)
            .map_err(|e| format!("failed to replace binary: {e}"))?;
    }

    staging_guard.disarm();
    Ok((info.version.clone(), current_exe))
}

/// Spawns the newly updated executable so the user seamlessly restarts into the new version.
pub fn relaunch_exe(exe: &Path) -> Result<(), String> {
    Command::new(exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to relaunch: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semver_variants() {
        assert_eq!(parse_semver("0.7.0"), Some((0, 7, 0)));
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("V2.10.4-rc1"), Some((2, 10, 4)));
        assert_eq!(parse_semver("not-a-version"), None);
    }

    #[test]
    fn compares_versions_correctly() {
        assert!(is_newer_version("0.7.0", "0.7.1"));
        assert!(is_newer_version("0.7.0", "v0.8.0"));
        assert!(is_newer_version("0.7.0", "1.0.0"));
        assert!(!is_newer_version("0.7.0", "v0.7.0"));
        assert!(!is_newer_version("0.7.0", "0.6.9"));
    }

    #[test]
    fn parses_github_latest_release_payload() {
        let sample = r#"{
            "url": "https://api.github.com/repos/roby-dev/roby-term/releases/123",
            "html_url": "https://github.com/roby-dev/roby-term/releases/tag/v0.8.0",
            "tag_name": "v0.8.0",
            "assets": [
                {
                    "name": "roby-term-macos-arm64",
                    "browser_download_url": "https://github.com/roby-dev/roby-term/releases/download/v0.8.0/roby-term-macos-arm64"
                },
                {
                    "name": "roby-term-windows-x64.exe",
                    "browser_download_url": "https://github.com/roby-dev/roby-term/releases/download/v0.8.0/roby-term-windows-x64.exe"
                }
            ]
        }"#;

        let win = parse_github_release_json(sample, "roby-term-windows-x64.exe").unwrap();
        assert_eq!(win.tag, "v0.8.0");
        assert_eq!(win.version, "0.8.0");
        assert_eq!(
            win.html_url,
            "https://github.com/roby-dev/roby-term/releases/tag/v0.8.0"
        );
        assert!(win.download_url.ends_with("roby-term-windows-x64.exe"));

        let mac = parse_github_release_json(sample, "roby-term-macos-arm64").unwrap();
        assert!(mac.download_url.ends_with("roby-term-macos-arm64"));
    }

    #[test]
    fn rejects_html_as_binary_header() {
        assert!(!validate_binary_header(b"<!DOCTYPE html>"));
        #[cfg(target_os = "windows")]
        assert!(validate_binary_header(b"MZ\x90\x00"));
    }

    #[test]
    fn ignores_untrusted_download_url_host() {
        let sample = r#"{
            "tag_name": "v0.9.0",
            "html_url": "https://github.com/roby-dev/roby-term/releases/tag/v0.9.0",
            "assets": [
                {
                    "name": "roby-term-windows-x64.exe",
                    "browser_download_url": "https://evil.example.com/roby-term-windows-x64.exe"
                }
            ]
        }"#;
        let info = parse_github_release_json(sample, "roby-term-windows-x64.exe").unwrap();
        assert_eq!(
            info.download_url,
            "https://github.com/roby-dev/roby-term/releases/download/v0.9.0/roby-term-windows-x64.exe"
        );
    }

    #[test]
    fn header_pill_shows_for_install_error_only() {
        let install_err = UpdateStatus::Error {
            message: "network error".into(),
            from_install: true,
        };
        let check_err = UpdateStatus::Error {
            message: "offline".into(),
            from_install: false,
        };
        assert!(install_err.shows_header_pill());
        assert!(!check_err.shows_header_pill());
    }

    #[test]
    fn preserves_utf8_in_json_strings() {
        let payload = r#"{"tag_name": "v1.0.0", "name": "Versión rápida 🚀"}"#;
        let (name, _) = extract_json_string_after(payload, "name").unwrap();
        assert_eq!(name, "Versión rápida 🚀");
    }

    #[test]
    fn reads_file_header_prefix_only() {
        let path = std::env::temp_dir().join("roby_term_test_header.bin");
        fs::write(&path, b"MZ\x90\x00EXTRA_BYTES_HERE").unwrap();
        let hdr = read_file_header(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(&hdr[..4], b"MZ\x90\x00");
        assert!(hdr.len() <= 8);
    }

    #[test]
    fn rejects_traversal_query_and_spoofed_urls_and_tags() {
        assert!(!is_safe_release_tag("v9.0.0-x/../../evil"));
        assert!(!is_safe_release_tag("v1.0.0?foo=bar"));
        assert!(!is_safe_release_tag(".."));
        assert!(is_safe_release_tag("v0.8.0-rc.1"));

        let asset = "roby-term-windows-x64.exe";
        assert!(is_trusted_download_url(
            "https://github.com/roby-dev/roby-term/releases/download/v0.8.0/roby-term-windows-x64.exe",
            asset
        ));
        assert!(!is_trusted_download_url(
            "https://github.com/roby-dev/roby-term/releases/download/v1/../../../../attacker/evil/releases/download/v1/roby-term-windows-x64.exe",
            asset
        ));
        assert!(!is_trusted_download_url(
            "https://github.com/roby-dev/roby-term/releases/download/v1/evil?x=roby-term-windows-x64.exe",
            asset
        ));
        assert!(!is_trusted_download_url(
            "https://github.com/roby-dev/roby-term/releases/download/v1/evil-roby-term-windows-x64.exe",
            asset
        ));

        let evil_tag_json = r#"{
            "tag_name": "v9.0.0-x/../../../../attacker/evil/releases/download/v1",
            "html_url": "https://github.com/roby-dev/roby-term/releases/tag/v9.0.0",
            "assets": []
        }"#;
        assert!(parse_github_release_json(evil_tag_json, asset).is_none());
    }

    #[test]
    fn skips_non_string_json_key_occurrences() {
        let payload = r#"{"html_url": null, "nested": {"html_url": "https://github.com/roby-dev/roby-term/releases/tag/v0.8.0"}, "tag_name": "v0.8.0"}"#;
        let info = parse_github_release_json(payload, "roby-term-windows-x64.exe").unwrap();
        assert_eq!(
            info.html_url,
            "https://github.com/roby-dev/roby-term/releases/tag/v0.8.0"
        );
    }
}
