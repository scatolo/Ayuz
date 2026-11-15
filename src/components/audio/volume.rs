// Ayuz - Unofficial Control Center for Asus Laptops
// Copyright (C) 2026 Guido Philipp
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see https://www.gnu.org/licenses/.

use gtk4 as gtk;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SharedVolumeState {
    volume: i32,
    muted: bool,
    last_local_write: Option<Instant>,
}

impl SharedVolumeState {
    fn in_cooldown(&self, now: Instant, cooldown: Duration) -> bool {
        self.last_local_write
            .is_some_and(|t| now.saturating_duration_since(t) < cooldown)
    }

    fn differs_from(&self, vol: f64, muted: bool) -> bool {
        (self.volume, self.muted) != (vol.round() as i32, muted)
    }
}

enum VolumeCmd {
    SetVolume(i32),
    ToggleMute,
}

pub struct VolumeModel {
    volume: f64,
    muted: bool,
    shared: Arc<Mutex<SharedVolumeState>>,
    cmd_tx: mpsc::UnboundedSender<VolumeCmd>,
}

#[derive(Debug)]
pub enum VolumeMsg {
    SetVolume(f64),
    ToggleMute,
    UpdateUi { vol: f64, muted: bool },
    LoadProfile(f64),
    Refresh,
}

// SimpleComponent is intentional here: volume control needs no async CommandOutput or
// error forwarding to the parent - it handles all async work via tokio::spawn internally.
#[relm4::component(pub)]
impl SimpleComponent for VolumeModel {
    type Init = ();
    type Input = VolumeMsg;
    type Output = String;

    view! {
        adw::PreferencesGroup {
            set_title: &gtk::glib::markup_escape_text(&t!("volume_booster_title")),
            set_description: Some(&t!("volume_booster_desc")),

            add = &adw::ActionRow {
                set_title: &t!("volume_level_label"),

                add_suffix = &gtk::ToggleButton {
                    set_valign: gtk::Align::Center,
                    add_css_class: "flat",
                    #[watch]
                    set_icon_name: if model.muted {
                        "audio-volume-muted-symbolic"
                    } else {
                        "audio-volume-high-symbolic"
                    },
                    #[watch]
                    #[block_signal(toggle_handler)]
                    set_active: model.muted,
                    connect_toggled[sender] => move |_| {
                        sender.input(VolumeMsg::ToggleMute);
                    } @toggle_handler,
                },

                add_suffix = &gtk::Scale {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_range: (0.0, 150.0),
                    #[watch]
                    #[block_signal(vol_handler)]
                    set_value: model.volume,
                    set_width_request: 200,
                    connect_value_changed[sender] => move |scale| {
                        sender.input(VolumeMsg::SetVolume(scale.value()));
                    } @vol_handler,
                },

                add_suffix = &gtk::Label {
                    #[watch]
                    set_label: &format!("{}%", model.volume.round() as i32),
                    set_width_chars: 5,
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let shared = Arc::new(Mutex::new(SharedVolumeState {
            volume: 100,
            muted: false,
            last_local_write: None,
        }));
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        tokio::spawn(volume_cmd_worker(cmd_rx, Arc::clone(&shared)));

        let sender_clone = sender.clone();
        tokio::spawn(async move {
            let (vol, muted) = read_current_volume().await.unwrap_or((100.0, false));
            sender_clone.input(VolumeMsg::UpdateUi { vol, muted });
        });

        tokio::spawn(start_volume_listener(sender.clone(), Arc::clone(&shared)));

        let model = VolumeModel {
            volume: 100.0,
            muted: false,
            shared,
            cmd_tx,
        };
        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: VolumeMsg, sender: ComponentSender<Self>) {
        match msg {
            VolumeMsg::UpdateUi { vol, muted } => {
                if !vol.is_finite() {
                    return;
                }
                let rounded = vol.round();
                let rounded_i32 = rounded as i32;
                {
                    let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                    s.volume = rounded_i32;
                    s.muted = muted;
                }
                if rounded_i32 == (self.volume.round() as i32) && muted == self.muted {
                    return;
                }
                self.volume = rounded;
                self.muted = muted;
            }
            VolumeMsg::SetVolume(vol) => {
                if !vol.is_finite() {
                    return;
                }
                let rounded = vol.clamp(0.0, 150.0).round();
                let target = rounded as i32;
                if target == (self.volume.round() as i32) {
                    return;
                }
                self.volume = rounded;
                self.muted = false;
                {
                    let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                    s.volume = target;
                    s.muted = false;
                    s.last_local_write = Some(Instant::now());
                }
                crate::services::config::AppConfig::update(|c| {
                    c.active_profile_mut().volume = rounded;
                });
                let _ = self.cmd_tx.send(VolumeCmd::SetVolume(target));
            }
            VolumeMsg::ToggleMute => {
                self.muted = !self.muted;
                {
                    let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                    s.muted = self.muted;
                    s.last_local_write = Some(Instant::now());
                }
                let _ = self.cmd_tx.send(VolumeCmd::ToggleMute);
            }
            VolumeMsg::LoadProfile(vol) => {
                if !vol.is_finite() {
                    return;
                }
                let rounded = vol.clamp(0.0, 150.0).round();
                let target = rounded as i32;
                if target == (self.volume.round() as i32) {
                    return;
                }
                self.volume = rounded;
                self.muted = false;
                {
                    let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                    s.volume = target;
                    s.muted = false;
                    s.last_local_write = Some(Instant::now());
                }
                let _ = self.cmd_tx.send(VolumeCmd::SetVolume(target));
            }
            VolumeMsg::Refresh => {
                let sender = sender.clone();
                tokio::spawn(async move {
                    if let Some((vol, muted)) = read_current_volume().await {
                        sender.input(VolumeMsg::UpdateUi { vol, muted });
                    }
                });
            }
        }
    }
}

async fn volume_cmd_worker(
    mut rx: mpsc::UnboundedReceiver<VolumeCmd>,
    shared: Arc<Mutex<SharedVolumeState>>,
) {
    while let Some(mut cmd) = rx.recv().await {
        while matches!(cmd, VolumeCmd::SetVolume(_)) {
            match rx.try_recv() {
                Ok(next @ VolumeCmd::SetVolume(_)) => cmd = next,
                Ok(other) => {
                    apply_volume_cmd(cmd, &shared).await;
                    cmd = other;
                    break;
                }
                Err(_) => break,
            }
        }
        apply_volume_cmd(cmd, &shared).await;
    }
}

async fn apply_volume_cmd(cmd: VolumeCmd, shared: &Mutex<SharedVolumeState>) {
    match cmd {
        VolumeCmd::SetVolume(target) => {
            let _ = tokio::process::Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "0"])
                .status()
                .await;
            let _ = tokio::process::Command::new("wpctl")
                .args([
                    "set-volume",
                    "@DEFAULT_AUDIO_SINK@",
                    &format!("{target}%"),
                ])
                .status()
                .await;
        }
        VolumeCmd::ToggleMute => {
            let _ = tokio::process::Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                .status()
                .await;
        }
    }
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    s.last_local_write = Some(Instant::now());
}

enum ReadVolumeStatus {
    Ok(f64, bool),
    BinaryMissing,
    TransientError,
}

/// Parses `wpctl get-volume` output (e.g. `"Volume: 0.45"` or `"Volume: 0.45 [MUTED]"`)
/// into `(percentage, muted)`, rounding percentage to the nearest integer.
pub(crate) fn parse_wpctl_volume(text: &str) -> Option<(f64, bool)> {
    let mut parts = text.split_whitespace();
    if parts.next()? != "Volume:" {
        return None;
    }
    let vol_str = parts.next()?;
    let val = vol_str.parse::<f64>().ok()?;
    if !val.is_finite() || val < 0.0 {
        return None;
    }
    let muted = text.contains("[MUTED]");
    Some(((val * 100.0).round(), muted))
}

async fn read_current_volume_status() -> ReadVolumeStatus {
    let out = match tokio::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .await
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ReadVolumeStatus::BinaryMissing;
        }
        Err(_) => return ReadVolumeStatus::TransientError,
    };
    if !out.status.success() {
        return ReadVolumeStatus::TransientError;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match parse_wpctl_volume(&text) {
        Some((vol, muted)) => ReadVolumeStatus::Ok(vol, muted),
        None => ReadVolumeStatus::TransientError,
    }
}

async fn read_current_volume() -> Option<(f64, bool)> {
    match read_current_volume_status().await {
        ReadVolumeStatus::Ok(vol, muted) => Some((vol, muted)),
        ReadVolumeStatus::BinaryMissing | ReadVolumeStatus::TransientError => None,
    }
}

async fn start_volume_listener(
    sender: relm4::ComponentSender<VolumeModel>,
    shared: Arc<Mutex<SharedVolumeState>>,
) {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;
    use tokio::time::sleep;

    if let Ok(mut child) = Command::new("pactl")
        .arg("subscribe")
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        && let Some(stdout) = child.stdout.take()
    {
        // Capacity 1: extra try_send calls during a burst are silently dropped
        let (tx, mut rx) = mpsc::channel::<()>(1);
        let debounce_sender = sender.clone();

        tokio::spawn(async move {
            const DEBOUNCE: Duration = Duration::from_millis(250);
            while rx.recv().await.is_some() {
                // Drain any signals that piled up before
                while rx.try_recv().is_ok() {}
                // Reset the 250 ms timer every time a new signal arrives
                loop {
                    tokio::select! {
                        _ = sleep(DEBOUNCE) => break,
                        msg = rx.recv() => {
                            if msg.is_none() { return; }
                            while rx.try_recv().is_ok() {}
                        }
                    }
                }
                if let Some((vol, muted)) = read_current_volume().await {
                    debounce_sender.input(VolumeMsg::UpdateUi { vol, muted });
                }
            }
        });

        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("'change'") && (line.contains("sink") || line.contains("server")) {
                let _ = tx.try_send(());
            }
        }
        let _ = child.wait().await;
    }

    poll_volume_loop(sender, shared).await;
}

async fn poll_volume_loop(
    sender: relm4::ComponentSender<VolumeModel>,
    shared: Arc<Mutex<SharedVolumeState>>,
) {
    const POLL_INTERVAL: Duration = Duration::from_secs(1);
    const LOCAL_WRITE_COOLDOWN: Duration = Duration::from_millis(500);

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        let in_cooldown = {
            let s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.in_cooldown(Instant::now(), LOCAL_WRITE_COOLDOWN)
        };
        if in_cooldown {
            continue;
        }
        match read_current_volume_status().await {
            ReadVolumeStatus::Ok(vol, muted) => {
                let changed = {
                    let s = shared.lock().unwrap_or_else(|e| e.into_inner());
                    s.differs_from(vol, muted)
                };
                if changed {
                    sender.input(VolumeMsg::UpdateUi { vol, muted });
                }
            }
            ReadVolumeStatus::BinaryMissing => return,
            ReadVolumeStatus::TransientError => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wpctl_volume_standard_and_muted() {
        assert_eq!(parse_wpctl_volume("Volume: 0.45\n"), Some((45.0, false)));
        assert_eq!(
            parse_wpctl_volume("Volume: 0.57 [MUTED]\n"),
            Some((57.0, true))
        );
        assert_eq!(parse_wpctl_volume("Volume: 1.50\n"), Some((150.0, false)));
        assert_eq!(parse_wpctl_volume("Volume: 0.00\n"), Some((0.0, false)));
    }

    #[test]
    fn test_parse_wpctl_volume_avoids_float_truncation() {
        // 0.29 * 100.0 is 28.999999999999996 in IEEE-754; rounding must yield 29, not 28.
        assert_eq!(parse_wpctl_volume("Volume: 0.29\n"), Some((29.0, false)));
        assert_eq!(parse_wpctl_volume("Volume: 0.56\n"), Some((56.0, false)));
    }

    #[test]
    fn test_parse_wpctl_volume_invalid() {
        assert_eq!(parse_wpctl_volume(""), None);
        assert_eq!(parse_wpctl_volume("Volume:"), None);
        assert_eq!(parse_wpctl_volume("Volume: abc"), None);
        assert_eq!(parse_wpctl_volume("Volume: -0.1"), None);
        assert_eq!(parse_wpctl_volume("Volume: NaN"), None);
        assert_eq!(parse_wpctl_volume("Error: 0.50"), None);
    }

    #[test]
    fn test_shared_volume_state_sync_and_cooldown() {
        let now = Instant::now();
        let state = SharedVolumeState {
            volume: 80,
            muted: false,
            last_local_write: Some(now),
        };
        assert!(!state.differs_from(80.0, false));
        assert!(state.differs_from(50.0, false));
        assert!(state.differs_from(80.0, true));

        let cooldown = Duration::from_millis(500);
        assert!(state.in_cooldown(now + Duration::from_millis(100), cooldown));
        assert!(!state.in_cooldown(now + Duration::from_millis(600), cooldown));
    }
}
