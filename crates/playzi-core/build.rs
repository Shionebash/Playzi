use std::fs;
use std::path::Path;

fn read_social_hub_from_dotenv(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let content = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == "SOCIAL_HUB_URL"
        {
            let clean = v.trim().trim_matches('"').trim_matches('\'').trim();
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-env-changed=SOCIAL_HUB_URL");

    let env_path = Path::new("../../.env");
    if env_path.exists() {
        println!("cargo:rerun-if-changed={}", env_path.display());
    }

    let url = std::env::var("SOCIAL_HUB_URL")
        .ok()
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| read_social_hub_from_dotenv(env_path))
        .unwrap_or_default();

    println!("cargo:rustc-env=PLAYZI_BUILT_SOCIAL_HUB_URL={url}");
}
