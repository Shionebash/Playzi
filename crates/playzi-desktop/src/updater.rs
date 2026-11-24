use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use chrono::Utc;
use playzi_core::updater::{
    CURRENT_VERSION, check_latest_release, configured_repo, download_release_asset,
    launch_installer_and_relaunch,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub status: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_name: Option<String>,
    pub release_notes: Option<String>,
    pub release_url: Option<String>,
    pub asset_name: Option<String>,
    pub progress: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
    pub checked_at: Option<String>,
    pub manual: bool,
}

impl Default for UpdateStatus {
    fn default() -> Self {
        Self {
            status: "idle".to_string(),
            current_version: CURRENT_VERSION.to_string(),
            latest_version: None,
            release_name: None,
            release_notes: None,
            release_url: None,
            asset_name: None,
            progress: 0.0,
            downloaded_bytes: 0,
            total_bytes: 0,
            error: None,
            checked_at: None,
            manual: false,
        }
    }
}

#[derive(Clone)]
pub struct DesktopUpdater {
    state: Arc<RwLock<UpdateStatus>>,
    busy: Arc<AtomicBool>,
}

impl Default for DesktopUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopUpdater {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(UpdateStatus::default())),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn snapshot(&self) -> UpdateStatus {
        self.state
            .read()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    fn set_and_emit(&self, app: &AppHandle, mutate: impl FnOnce(&mut UpdateStatus)) -> UpdateStatus {
        let next = {
            let mut guard = self.state.write().expect("update state poisoned");
            mutate(&mut guard);
            guard.clone()
        };
        let _ = app.emit("update-status", &next);
        next
    }

    pub async fn check_and_install(
        &self,
        app: AppHandle,
        auto_install: bool,
        manual: bool,
    ) -> UpdateStatus {
        if self
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return self.snapshot();
        }

        let result = self.run_flow(&app, auto_install, manual).await;
        self.busy.store(false, Ordering::SeqCst);
        result
    }

    async fn run_flow(
        &self,
        app: &AppHandle,
        auto_install: bool,
        manual: bool,
    ) -> UpdateStatus {
        self.set_and_emit(app, |s| {
            s.status = "checking".to_string();
            s.error = None;
            s.progress = 0.0;
            s.downloaded_bytes = 0;
            s.total_bytes = 0;
            s.manual = manual;
        });

        let repo = configured_repo();
        let check = match check_latest_release(&repo, CURRENT_VERSION).await {
            Ok(res) => res,
            Err(err) => {
                return self.set_and_emit(app, |s| {
                    s.status = "error".to_string();
                    s.error = Some(err.to_string());
                    s.checked_at = Some(Utc::now().to_rfc3339());
                    s.manual = manual;
                });
            }
        };

        let Some(release) = check.release else {
            return self.set_and_emit(app, |s| {
                s.status = "up-to-date".to_string();
                s.latest_version = Some(CURRENT_VERSION.to_string());
                s.error = None;
                s.checked_at = Some(Utc::now().to_rfc3339());
                s.manual = manual;
            });
        };

        let status = self.set_and_emit(app, |s| {
            s.status = "available".to_string();
            s.latest_version = Some(release.version.clone());
            s.release_name = Some(release.name.clone());
            s.release_notes = Some(release.body.clone());
            s.release_url = Some(release.html_url.clone());
            s.asset_name = Some(release.asset_name.clone());
            s.total_bytes = release.asset_size;
            s.error = None;
            s.checked_at = Some(Utc::now().to_rfc3339());
            s.manual = manual;
        });

        if !auto_install {
            return status;
        }

        self.set_and_emit(app, |s| {
            s.status = "downloading".to_string();
            s.progress = 0.0;
            s.downloaded_bytes = 0;
            s.total_bytes = release.asset_size;
        });

        let mut last_emit = Instant::now();
        let downloaded_path = match download_release_asset(
            &release.download_url,
            &release.asset_name,
            release.asset_size,
            |downloaded, total, pct| {
                if last_emit.elapsed().as_millis() >= 200 || pct >= 100.0 {
                    last_emit = Instant::now();
                    self.set_and_emit(app, |s| {
                        s.status = "downloading".to_string();
                        s.downloaded_bytes = downloaded;
                        s.total_bytes = total;
                        s.progress = (pct * 10.0).round() / 10.0;
                    });
                }
            },
        )
        .await
        {
            Ok(path) => path,
            Err(err) => {
                return self.set_and_emit(app, |s| {
                    s.status = "error".to_string();
                    s.error = Some(err.to_string());
                });
            }
        };

        let installing_status = self.set_and_emit(app, |s| {
            s.status = "installing".to_string();
            s.progress = 100.0;
            s.error = None;
        });

        let current_exe = std::env::current_exe().ok();
        if let Err(err) = launch_installer_and_relaunch(&downloaded_path, current_exe.as_deref()) {
            return self.set_and_emit(app, |s| {
                s.status = "error".to_string();
                s.error = Some(err.to_string());
            });
        }

        // Give the frontend a brief moment to show the installing notice before exiting
        let app_exit = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            app_exit.exit(0);
        });

        installing_status
    }
}
