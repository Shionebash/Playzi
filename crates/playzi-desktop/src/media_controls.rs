use std::time::Duration;

use playzi_core::player::{PlaybackState, PlayerControlCommand, PlayerManager};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use tauri::WebviewWindow;

/// Extracts the native window handle (`HWND` on Windows) as `isize` for SMTC and `mpv --wid`.
pub fn window_hwnd_isize(window: &WebviewWindow) -> Option<isize> {
    #[cfg(target_os = "windows")]
    {
        window.hwnd().ok().map(|h| h.0 as isize)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        None
    }
}

/// Cross-platform OS media controls bridge (Windows SMTC / Linux MPRIS / macOS Now Playing).
pub struct OsMediaControls {
    controls: MediaControls,
}

impl OsMediaControls {
    pub fn attach(hwnd: Option<isize>, player: PlayerManager) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        let platform_hwnd = hwnd.map(|h| h as *mut std::ffi::c_void);
        #[cfg(not(target_os = "windows"))]
        let platform_hwnd = {
            let _ = hwnd;
            None
        };

        let config = PlatformConfig {
            dbus_name: "playzi",
            display_name: "Playzi",
            hwnd: platform_hwnd,
        };

        let mut controls = MediaControls::new(config).map_err(|e| format!("{e:?}"))?;
        controls
            .attach(move |event: MediaControlEvent| {
                match event {
                    MediaControlEvent::Play => {
                        let _ = player.control(PlayerControlCommand::Pause { paused: false });
                    }
                    MediaControlEvent::Pause => {
                        let _ = player.control(PlayerControlCommand::Pause { paused: true });
                    }
                    MediaControlEvent::Toggle => {
                        let _ = player.control(PlayerControlCommand::TogglePause);
                    }
                    MediaControlEvent::Stop => {
                        let _ = player.stop();
                    }
                    MediaControlEvent::SeekBy(direction, duration) => {
                        let secs = duration.as_secs_f64();
                        let signed = match direction {
                            SeekDirection::Forward => secs,
                            SeekDirection::Backward => -secs,
                        };
                        let _ = player.control(PlayerControlCommand::Seek {
                            seconds: signed,
                            relative: true,
                        });
                    }
                    MediaControlEvent::SetPosition(MediaPosition(duration)) => {
                        let _ = player.control(PlayerControlCommand::Seek {
                            seconds: duration.as_secs_f64(),
                            relative: false,
                        });
                    }
                    MediaControlEvent::SetVolume(vol) => {
                        let _ = player.control(PlayerControlCommand::SetVolume {
                            volume: (vol * 100.0).clamp(0.0, 150.0),
                        });
                    }
                    _ => {}
                }
            })
            .map_err(|e| format!("{e:?}"))?;

        Ok(Self { controls })
    }

    pub fn sync_state(&mut self, state: &PlaybackState) {
        if !state.active {
            let _ = self.controls.set_playback(MediaPlayback::Stopped);
            return;
        }

        let duration = if state.duration_secs.is_finite() && state.duration_secs > 0.0 {
            Some(Duration::from_secs_f64(state.duration_secs))
        } else {
            None
        };
        let title = if state.title.trim().is_empty() {
            "Playzi"
        } else {
            state.title.as_str()
        };

        let _ = self.controls.set_metadata(MediaMetadata {
            title: Some(title),
            artist: Some("Playzi"),
            album: None,
            cover_url: None,
            duration,
        });

        let pos_secs = if state.position_secs.is_finite() {
            state.position_secs.max(0.0)
        } else {
            0.0
        };
        let progress = Some(MediaPosition(Duration::from_secs_f64(pos_secs)));
        let playback = if state.paused {
            MediaPlayback::Paused { progress }
        } else {
            MediaPlayback::Playing { progress }
        };
        let _ = self.controls.set_playback(playback);
    }
}
