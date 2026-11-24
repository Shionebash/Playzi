use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub update_available: bool,
    pub release: Option<ReleaseInfo>,
}

pub fn configured_repo() -> String {
    std::env::var("PLAYZI_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
        .build()
        .map_err(CoreError::from)
}

pub async fn check_latest_release(
    repo: &str,
    current_version: &str,
) -> Result<UpdateCheckResult> {
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

pub async fn download_release_asset<F>(
    download_url: &str,
    asset_name: &str,
    expected_size: u64,
    mut on_progress: F,
) -> Result<PathBuf>
where
    F: FnMut(u64, u64, f64),
{
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
    let updates_dir = std::env::temp_dir().join("playzi-updates");
    tokio::fs::create_dir_all(&updates_dir).await?;

    let safe_name = sanitize_filename(asset_name);
    let final_path = updates_dir.join(&safe_name);
    let part_path = updates_dir.join(format!("{safe_name}.part"));

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
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;

        let installer_str = installer_path.to_string_lossy().to_string();
        let lower = installer_str.to_ascii_lowercase();
        let relaunch_str = current_exe
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let script_path = installer_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("apply-update.ps1");

        let install_cmd = if lower.ends_with(".msi") {
            format!(
                "Start-Process -FilePath 'msiexec.exe' -ArgumentList @('/i', '{}', '/passive') -Wait",
                ps_single_quote(&installer_str)
            )
        } else {
            format!(
                "Start-Process -FilePath '{}' -ArgumentList '/S' -Wait",
                ps_single_quote(&installer_str)
            )
        };

        let script = format!(
            "$ErrorActionPreference = 'SilentlyContinue'\n\
             Start-Sleep -Seconds 2\n\
             {install_cmd}\n\
             Start-Sleep -Seconds 1\n\
             $candidates = @(\n\
               '{relaunch}',\n\
               (Join-Path $env:LOCALAPPDATA 'Playzi\\playzi-desktop.exe'),\n\
               (Join-Path $env:LOCALAPPDATA 'Programs\\Playzi\\playzi-desktop.exe'),\n\
               (Join-Path $env:ProgramFiles 'Playzi\\playzi-desktop.exe')\n\
             )\n\
             foreach ($exe in $candidates) {{\n\
               if ($exe -and (Test-Path -LiteralPath $exe)) {{\n\
                 Start-Process -FilePath $exe\n\
                 break\n\
               }}\n\
             }}\n",
            install_cmd = install_cmd,
            relaunch = ps_single_quote(&relaunch_str),
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
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
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
}
