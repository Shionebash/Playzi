use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use url::Url;
use uuid::Uuid;

use crate::error::{CoreError, Result};

pub const DEFAULT_GITHUB_REPO: &str = "Shionebash/Playzi";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseInfo {
    pub version: String,
    pub tag_name: String,
    pub name: String,
    pub body: String,
    pub html_url: String,
    pub published_at: Option<String>,
    pub asset_name: String,
    pub download_url: String,
    pub asset_size: u64,
    #[serde(default)]
    pub sha256_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub update_available: bool,
    pub release: Option<ReleaseInfo>,
}

pub fn is_valid_github_repo(repo: &str) -> bool {
    let mut parts = repo.trim().split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let valid_seg = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    valid_seg(owner) && valid_seg(name)
}

pub fn is_allowed_update_download_url(raw_url: &str) -> bool {
    let Ok(parsed) = Url::parse(raw_url.trim()) else {
        return false;
    };
    if parsed.scheme() != "https" {
        return false;
    }
    let Some(host) = parsed.host_str().map(|h| h.to_ascii_lowercase()) else {
        return false;
    };
    host == "github.com"
        || host == "api.github.com"
        || host == "githubusercontent.com"
        || host.ends_with(".githubusercontent.com")
}

pub fn configured_repo() -> String {
    std::env::var("PLAYZI_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| is_valid_github_repo(s))
        .unwrap_or_else(|| DEFAULT_GITHUB_REPO.to_string())
}

pub fn normalize_version(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .to_string()
}

pub fn parse_semver(raw: &str) -> Option<(u64, u64, u64)> {
    let cleaned = normalize_version(raw);
    let core = cleaned.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    match (parse_semver(current), parse_semver(candidate)) {
        (Some(cur), Some(cand)) => cand > cur,
        _ => false,
    }
}

pub fn select_asset_for_platform<'a>(
    assets: &'a [GitHubAsset],
    os: &str,
    arch: &str,
) -> Option<&'a GitHubAsset> {
    let arch_tokens: &[&str] = match arch {
        "x86_64" => &["x64", "x86_64", "amd64"],
        "aarch64" => &["arm64", "aarch64"],
        "x86" => &["x86", "i686", "ia32"],
        _ => &[],
    };

    let matches_arch = |name: &str| -> bool {
        let lower = name.to_ascii_lowercase();
        if arch_tokens.is_empty() {
            return true;
        }
        arch_tokens.iter().any(|tok| lower.contains(tok))
    };

    let is_ignored = |name: &str| -> bool {
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".sig")
            || lower.ends_with(".sha256")
            || lower.ends_with(".sha512")
            || lower.ends_with(".json")
            || lower.ends_with(".txt")
    };

    let pick = |predicate: &dyn Fn(&str) -> bool| -> Option<&'a GitHubAsset> {
        assets
            .iter()
            .find(|a| !is_ignored(&a.name) && predicate(&a.name) && matches_arch(&a.name))
            .or_else(|| {
                assets
                    .iter()
                    .find(|a| !is_ignored(&a.name) && predicate(&a.name))
            })
    };

    match os {
        "windows" => pick(&|n| n.to_ascii_lowercase().ends_with("-setup.exe"))
            .or_else(|| pick(&|n| n.to_ascii_lowercase().ends_with(".msi")))
            .or_else(|| pick(&|n| n.to_ascii_lowercase().ends_with(".exe"))),
        "linux" => pick(&|n| n.to_ascii_lowercase().ends_with(".appimage"))
            .or_else(|| pick(&|n| n.to_ascii_lowercase().ends_with(".deb"))),
        "macos" => pick(&|n| n.to_ascii_lowercase().ends_with(".dmg")),
        _ => None,
    }
}

fn build_http_client(timeout: Duration) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    let ua = format!("Playzi/{CURRENT_VERSION}");
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&ua).unwrap_or_else(|_| HeaderValue::from_static("Playzi/1.0")),
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );

    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many redirects");
            }
            if !is_allowed_update_download_url(attempt.url().as_str()) {
                return attempt.error("redirect to disallowed update host");
            }
            attempt.follow()
        }))
        .build()
        .map_err(CoreError::from)
}

pub async fn check_latest_release(
    repo: &str,
    current_version: &str,
) -> Result<UpdateCheckResult> {
    if !is_valid_github_repo(repo) {
        return Err(CoreError::Validation(format!(
            "Invalid GitHub update repository: {repo}"
        )));
    }
    let client = build_http_client(Duration::from_secs(20))?;
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let resp = client.get(&url).send().await?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(UpdateCheckResult {
            current_version: normalize_version(current_version),
            update_available: false,
            release: None,
        });
    }

    if !resp.status().is_success() {
        return Err(CoreError::External(format!(
            "GitHub Releases API returned HTTP {}",
            resp.status()
        )));
    }

    let release: GitHubRelease = resp.json().await?;
    if release.draft || release.prerelease {
        return Ok(UpdateCheckResult {
            current_version: normalize_version(current_version),
            update_available: false,
            release: None,
        });
    }

    let latest_version = normalize_version(&release.tag_name);
    if !is_newer_version(current_version, &latest_version) {
        return Ok(UpdateCheckResult {
            current_version: normalize_version(current_version),
            update_available: false,
            release: None,
        });
    }

    let asset = select_asset_for_platform(
        &release.assets,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
    .ok_or_else(|| {
        CoreError::NotFound(format!(
            "No compatible installer asset found in release {} for {}/{}",
            release.tag_name,
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;

    if !is_allowed_update_download_url(&asset.browser_download_url) {
        return Err(CoreError::Validation(
            "Release asset download URL is not on an allowed GitHub HTTPS host".into(),
        ));
    }

    let expected_sha_name = format!("{}.sha256", asset.name).to_ascii_lowercase();
    let sha256_url = release
        .assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase() == expected_sha_name)
        .map(|a| a.browser_download_url.clone())
        .filter(|u| is_allowed_update_download_url(u));

    Ok(UpdateCheckResult {
        current_version: normalize_version(current_version),
        update_available: true,
        release: Some(ReleaseInfo {
            version: latest_version,
            tag_name: release.tag_name.clone(),
            name: release.name.unwrap_or_else(|| release.tag_name.clone()),
            body: release.body.unwrap_or_default(),
            html_url: release.html_url,
            published_at: release.published_at,
            asset_name: asset.name.clone(),
            download_url: asset.browser_download_url.clone(),
            asset_size: asset.size,
            sha256_url,
        }),
    })
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "playzi-update-installer".to_string()
    } else {
        cleaned
    }
}

fn resolve_updates_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let trimmed = local_app_data.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("Playzi").join("updates");
        }
    }
    #[cfg(not(target_os = "windows"))]
    if let Ok(xdg_cache) = std::env::var("XDG_CACHE_HOME") {
        let trimmed = xdg_cache.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("playzi").join("updates");
        }
    }
    std::env::temp_dir().join(format!("playzi-updates-{}", std::process::id()))
}

pub async fn verify_asset_sha256(file_path: &Path, sha256_url: &str) -> Result<()> {
    if !is_allowed_update_download_url(sha256_url) {
        return Err(CoreError::Validation(
            "Update checksum URL is not on an allowed GitHub HTTPS host".into(),
        ));
    }
    let client = build_http_client(Duration::from_secs(30))?;
    let resp = client.get(sha256_url).send().await?;
    if !resp.status().is_success() {
        return Err(CoreError::External(format!(
            "Failed to download update SHA-256 checksum (HTTP {})",
            resp.status()
        )));
    }
    let text = resp.text().await?;
    let expected = text
        .split_whitespace()
        .next()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| CoreError::External("Invalid SHA-256 checksum format in release asset".into()))?;

    let path = file_path.to_path_buf();
    let actual = tokio::task::spawn_blocking(move || -> Result<String> {
        let bytes = std::fs::read(&path)?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .map_err(|e| CoreError::External(format!("Checksum task failed: {e}")))??;

    if actual != expected {
        let _ = tokio::fs::remove_file(file_path).await;
        return Err(CoreError::External(format!(
            "Update installer SHA-256 mismatch (expected {expected}, got {actual})"
        )));
    }
    Ok(())
}

pub async fn download_release_asset<F>(
    download_url: &str,
    asset_name: &str,
    expected_size: u64,
    mut on_progress: F,
) -> Result<PathBuf>
where
    F: FnMut(u64, u64, f64),
{
    if !is_allowed_update_download_url(download_url) {
        return Err(CoreError::Validation(
            "Update download URL is not on an allowed GitHub HTTPS host".into(),
        ));
    }
    let client = build_http_client(Duration::from_secs(600))?;
    let mut resp = client
        .get(download_url)
        .header(ACCEPT, "application/octet-stream")
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(CoreError::External(format!(
            "Failed to download update asset (HTTP {})",
            resp.status()
        )));
    }

    let total_bytes = resp.content_length().unwrap_or(expected_size);
    let updates_dir = resolve_updates_dir();
    tokio::fs::create_dir_all(&updates_dir).await?;

    let safe_name = sanitize_filename(asset_name);
    let final_path = updates_dir.join(&safe_name);
    let part_path = updates_dir.join(format!("{safe_name}.{}.part", Uuid::new_v4()));

    let mut file = tokio::fs::File::create(&part_path).await?;
    let mut downloaded: u64 = 0;

    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        let pct = if total_bytes > 0 {
            ((downloaded as f64 / total_bytes as f64) * 100.0).min(100.0)
        } else {
            0.0
        };
        on_progress(downloaded, total_bytes, pct);
    }

    file.flush().await?;
    drop(file);

    if expected_size > 0 && downloaded != expected_size {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(CoreError::External(format!(
            "Downloaded update size mismatch (expected {expected_size} bytes, got {downloaded} bytes)"
        )));
    }

    if final_path.exists() {
        let _ = tokio::fs::remove_file(&final_path).await;
    }
    tokio::fs::rename(&part_path, &final_path).await?;
    Ok(final_path)
}

#[cfg(target_os = "windows")]
fn ps_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(target_os = "windows")]
fn build_windows_update_script(
    installer_path: &Path,
    relaunch_exe: Option<&Path>,
    parent_pid: u32,
    log_path: &Path,
) -> String {
    let installer_str = installer_path.to_string_lossy().to_string();
    let lower = installer_str.to_ascii_lowercase();
    let relaunch_str = relaunch_exe
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let log_str = log_path.to_string_lossy().to_string();

    let install_cmd = if lower.ends_with(".msi") {
        format!(
            "$proc = Start-Process -FilePath 'msiexec.exe' -ArgumentList @('/i', '\"{}\"', '/passive', '/norestart') -Wait -PassThru",
            ps_single_quote(&installer_str)
        )
    } else {
        format!(
            "$proc = Start-Process -FilePath '{}' -ArgumentList @('/S', '/UPDATE') -Wait -PassThru",
            ps_single_quote(&installer_str)
        )
    };

    format!(
        "$ErrorActionPreference = 'Continue'\n\
         $logPath = '{log_path}'\n\
         function Write-UpdateLog([string]$msg) {{\n\
           try {{\n\
             $ts = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')\n\
             Add-Content -LiteralPath $logPath -Value \"[$ts] $msg\" -Encoding UTF8\n\
           }} catch {{}}\n\
         }}\n\
         Write-UpdateLog 'Starting update helper (parent PID {parent_pid})'\n\
         $deadline = (Get-Date).AddSeconds(15)\n\
         while ((Get-Date) -lt $deadline) {{\n\
           if (-not (Get-Process -Id {parent_pid} -ErrorAction SilentlyContinue)) {{ break }}\n\
           Start-Sleep -Milliseconds 250\n\
         }}\n\
         if (Get-Process -Id {parent_pid} -ErrorAction SilentlyContinue) {{\n\
           Write-UpdateLog 'Parent process still running after 15s; forcing stop'\n\
           Stop-Process -Id {parent_pid} -Force -ErrorAction SilentlyContinue\n\
           Start-Sleep -Milliseconds 500\n\
         }}\n\
         Start-Sleep -Milliseconds 500\n\
         Write-UpdateLog 'Running installer: {installer}'\n\
         try {{\n\
           {install_cmd}\n\
           $exitCode = if ($proc) {{ $proc.ExitCode }} else {{ -1 }}\n\
           Write-UpdateLog \"Installer finished with exit code $exitCode\"\n\
         }} catch {{\n\
           Write-UpdateLog \"Installer failed: $($_.Exception.Message)\"\n\
         }}\n\
         Start-Sleep -Seconds 1\n\
         $regInstallDir = ''\n\
         try {{\n\
           $regInstallDir = (Get-ItemProperty -Path 'HKCU:\\Software\\playzi\\Playzi' -ErrorAction SilentlyContinue).'(default)'\n\
         }} catch {{}}\n\
         $candidates = @(\n\
           '{relaunch}',\n\
           $(if ($regInstallDir) {{ Join-Path $regInstallDir 'playzi-desktop.exe' }} else {{ '' }}),\n\
           (Join-Path $env:LOCALAPPDATA 'Playzi\\playzi-desktop.exe'),\n\
           (Join-Path $env:LOCALAPPDATA 'Programs\\Playzi\\playzi-desktop.exe'),\n\
           (Join-Path $env:ProgramFiles 'Playzi\\playzi-desktop.exe')\n\
         )\n\
         foreach ($exe in $candidates) {{\n\
           if ($exe -and (Test-Path -LiteralPath $exe)) {{\n\
             Write-UpdateLog \"Relaunching $exe\"\n\
             Start-Process -FilePath $exe\n\
             break\n\
           }}\n\
         }}\n",
        log_path = ps_single_quote(&log_str),
        parent_pid = parent_pid,
        installer = ps_single_quote(&installer_str),
        install_cmd = install_cmd,
        relaunch = ps_single_quote(&relaunch_str),
    )
}

pub fn launch_installer_and_relaunch(
    installer_path: &Path,
    current_exe: Option<&Path>,
) -> Result<()> {
    if !installer_path.exists() {
        return Err(CoreError::NotFound(format!(
            "Installer not found at {}",
            installer_path.display()
        )));
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Stdio;
        // Do NOT combine with DETACHED_PROCESS: Windows PowerShell aborts
        // silently with exit code 0 before executing `-File` when launched
        // with DETACHED_PROCESS. CREATE_NO_WINDOW provides a hidden console
        // host while CREATE_NEW_PROCESS_GROUP detaches from parent signals.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

        let parent_dir = installer_path.parent().unwrap_or_else(|| Path::new("."));
        let script_path = parent_dir.join("apply-update.ps1");
        let log_path = parent_dir.join("update.log");

        let script = build_windows_update_script(
            installer_path,
            current_exe,
            std::process::id(),
            &log_path,
        );

        std::fs::write(&script_path, script)?;

        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-File",
            ])
            .arg(&script_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
            .spawn()?;

        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let lower = installer_path.to_string_lossy().to_ascii_lowercase();
        if lower.ends_with(".appimage") {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(installer_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(installer_path, perms)?;
            std::process::Command::new(installer_path).spawn()?;
        } else {
            std::process::Command::new("xdg-open")
                .arg(installer_path)
                .spawn()?;
        }
        let _ = current_exe;
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(installer_path)
            .spawn()?;
        let _ = current_exe;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(CoreError::External(
        "Automatic installation is not supported on this operating system".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_handles_v_prefix_and_numeric_ordering() {
        assert_eq!(parse_semver("1.0.1"), Some((1, 0, 1)));
        assert_eq!(parse_semver("v1.0.10"), Some((1, 0, 10)));
        assert_eq!(parse_semver("V2.3.4-beta.1"), Some((2, 3, 4)));

        assert!(is_newer_version("1.0.1", "1.0.2"));
        assert!(is_newer_version("1.0.2", "v1.0.10"));
        assert!(is_newer_version("1.0.9", "1.1.0"));
        assert!(is_newer_version("1.9.9", "2.0.0"));

        assert!(!is_newer_version("1.0.2", "1.0.2"));
        assert!(!is_newer_version("1.0.2", "v1.0.1"));
        assert!(!is_newer_version("1.2.0", "1.1.9"));
    }

    #[test]
    fn selects_windows_nsis_setup_installer_first() {
        let assets = vec![
            GitHubAsset {
                name: "Playzi_1.0.2_x64-setup.exe.sig".to_string(),
                browser_download_url: "https://example.com/sig".to_string(),
                size: 128,
            },
            GitHubAsset {
                name: "Playzi_1.0.2_x64_en-US.msi".to_string(),
                browser_download_url: "https://example.com/msi".to_string(),
                size: 1000,
            },
            GitHubAsset {
                name: "Playzi_1.0.2_x64-setup.exe".to_string(),
                browser_download_url: "https://example.com/nsis".to_string(),
                size: 2000,
            },
        ];

        let chosen = select_asset_for_platform(&assets, "windows", "x86_64").unwrap();
        assert_eq!(chosen.name, "Playzi_1.0.2_x64-setup.exe");
    }

    #[test]
    fn selects_linux_appimage_or_deb() {
        let assets = vec![
            GitHubAsset {
                name: "playzi_1.0.2_amd64.deb".to_string(),
                browser_download_url: "https://example.com/deb".to_string(),
                size: 1000,
            },
            GitHubAsset {
                name: "Playzi_1.0.2_amd64.AppImage".to_string(),
                browser_download_url: "https://example.com/appimage".to_string(),
                size: 2000,
            },
        ];

        let chosen = select_asset_for_platform(&assets, "linux", "x86_64").unwrap();
        assert_eq!(chosen.name, "Playzi_1.0.2_amd64.AppImage");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_update_script_waits_for_parent_and_passes_nsis_update_flags() {
        let script = build_windows_update_script(
            Path::new(r"C:\Temp\playzi-updates\Playzi_1.0.4_x64-setup.exe"),
            Some(Path::new(r"C:\Users\Test\AppData\Local\Playzi\playzi-desktop.exe")),
            4242,
            Path::new(r"C:\Temp\playzi-updates\update.log"),
        );

        assert!(script.contains("Get-Process -Id 4242"));
        assert!(script.contains("@('/S', '/UPDATE')"));
        assert!(script.contains("update.log"));
        assert!(script.contains(r"C:\Users\Test\AppData\Local\Playzi\playzi-desktop.exe"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_update_script_quotes_msi_installer_path() {
        let script = build_windows_update_script(
            Path::new(r"C:\Users\My User\AppData\Local\Temp\Playzi_1.0.4_x64.msi"),
            None,
            1234,
            Path::new(r"C:\Users\My User\AppData\Local\Temp\update.log"),
        );

        assert!(script.contains("'msiexec.exe'"));
        assert!(script.contains(r##"'"C:\Users\My User\AppData\Local\Temp\Playzi_1.0.4_x64.msi"'"##));
        assert!(script.contains("'/norestart'"));
    }

    #[test]
    fn validates_github_repo_and_download_urls() {
        assert!(is_valid_github_repo("Shionebash/Playzi"));
        assert!(!is_valid_github_repo("Shionebash/Playzi/extra"));
        assert!(!is_valid_github_repo("../evil"));
        assert!(!is_valid_github_repo("owner/repo?foo=bar"));

        assert!(is_allowed_update_download_url(
            "https://github.com/Shionebash/Playzi/releases/download/v1.2.15/Playzi.exe"
        ));
        assert!(is_allowed_update_download_url(
            "https://objects.githubusercontent.com/github-production-release-asset/123"
        ));
        assert!(!is_allowed_update_download_url(
            "http://github.com/Shionebash/Playzi/releases/download/v1.2.15/Playzi.exe"
        ));
        assert!(!is_allowed_update_download_url(
            "https://evil.example.com/github.com/Playzi.exe"
        ));
    }
}
