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
use gtk4::gio;
use gtk4::glib;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::services::commands::run_command_blocking;
use crate::services::config::AppConfig;

const PRESET_MUSIC: &str = include_str!("../../../assets/presets/Music.json");
const PRESET_MOVIE: &str = include_str!("../../../assets/presets/Movie.json");
const PRESET_VIDEO: &str = include_str!("../../../assets/presets/Video.json");
const PRESET_VOICE: &str = include_str!("../../../assets/presets/Voice.json");
const PRESET_CUSTOM_EQ: &str = include_str!("../../../assets/presets/Perfect_EQ.json");

const PRESETS: &[(&str, &str)] = &[
    ("Movie", PRESET_MOVIE),
    ("Music", PRESET_MUSIC),
    ("Perfect_EQ", PRESET_CUSTOM_EQ),
    ("Video", PRESET_VIDEO),
    ("Voice", PRESET_VOICE),
];

// Index 0..6: Movie, Music, None(bypass), Perfect_EQ, Video, Voice, Custom
// Index 2 = None (no preset, bypass only)
const NONE_IDX: u32 = 2;
const CUSTOM_IDX: u32 = 6;
const PRESET_NAMES: &[&str] = &["Movie", "Music", "Perfect_EQ", "Video", "Voice"];
const EASYEFFECTS_STARTUP_DELAY_MS: u64 = 1500;

static EASYEFFECTS_MAJOR_VERSION: OnceLock<u32> = OnceLock::new();

pub struct SoundModesModel {
    ee_installed: bool,
    current_profile: u32,
    previous_profile: u32,
    dropdown: gtk::DropDown,
}

#[derive(Debug)]
pub enum SoundModesMsg {
    ChangeProfile(u32),
    CustomPresetPathSelected(PathBuf),
    CustomCancelled(u32),
    LoadProfile(u32),
}

#[derive(Debug)]
pub enum AudioCommandOutput {
    EeChecked(bool),
    PresetsInstalled,
    ProfileSet(u32),
    CustomPresetLoaded(String),
    Error(String),
}

#[relm4::component(pub)]
impl Component for SoundModesModel {
    type Init = ();
    type Input = SoundModesMsg;
    type Output = String;
    type CommandOutput = AudioCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &t!("audio_profiles_title"),
            set_description: Some(&t!("audio_profiles_desc")),

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                #[watch]
                set_visible: !model.ee_installed,
                set_label: &t!("ee_missing_warning"),
            },

            add = &adw::ActionRow {
                set_title: &t!("audio_profile_label"),
                add_suffix = &model.dropdown.clone(),
                set_activatable_widget: Some(&model.dropdown),
                #[watch]
                set_sensitive: model.ee_installed,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = AppConfig::load();

        let options = gtk::StringList::new(&[
            &t!("audio_profile_film"),
            &t!("audio_profile_music"),
            &t!("audio_profile_none"),
            &t!("audio_profile_optimized"),
            &t!("audio_profile_video"),
            &t!("audio_profile_voice"),
            &t!("audio_profile_custom"),
        ]);
        let dropdown = gtk::DropDown::new(Some(options), gtk::Expression::NONE);
        dropdown.set_valign(gtk::Align::Center);
        let saved_audio = config.active_profile().audio_profile;
        dropdown.set_selected(saved_audio);

        {
            let sender = sender.clone();
            dropdown.connect_selected_notify(move |dd| {
                sender.input(SoundModesMsg::ChangeProfile(dd.selected()));
            });
        }

        let model = SoundModesModel {
            ee_installed: false,
            current_profile: saved_audio,
            previous_profile: saved_audio,
            dropdown,
        };

        let widgets = view_output!();

        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    let installed =
                        crate::services::commands::which_exists("easyeffects").await;
                    out.emit(AudioCommandOutput::EeChecked(installed));
                    if installed {
                        match install_presets().await {
                            Ok(()) => out.emit(AudioCommandOutput::PresetsInstalled),
                            Err(e) => out.emit(AudioCommandOutput::Error(
                                t!("audio_preset_install_error", error = e).to_string(),
                            )),
                        }
                    }
                })
                .drop_on_shutdown()
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: SoundModesMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            SoundModesMsg::ChangeProfile(idx) => {
                if idx == self.current_profile {
                    return;
                }

                if idx == CUSTOM_IDX {
                    let previous = self.current_profile;
                    self.current_profile = CUSTOM_IDX;

                    let sender_clone = sender.clone();
                    let dialog = gtk::FileDialog::builder()
                        .title(t!("audio_profile_custom").as_ref())
                        .accept_label("Open")
                        .build();
                    let filter = gtk::FileFilter::new();
                    filter.add_pattern("*.json");
                    filter.set_name(Some("JSON"));
                    let store = gio::ListStore::new::<gtk::FileFilter>();
                    store.append(&filter);
                    dialog.set_filters(Some(&store));

                    glib::spawn_future_local(async move {
                        match dialog.open_future(None::<&gtk::Window>).await {
                            Ok(file) => {
                                if let Some(path) = file.path() {
                                    sender_clone.input(SoundModesMsg::CustomPresetPathSelected(path));
                                } else {
                                    sender_clone.input(SoundModesMsg::CustomCancelled(previous));
                                }
                            }
                            Err(_) => {
                                sender_clone.input(SoundModesMsg::CustomCancelled(previous));
                            }
                        }
                    });
                    return;
                }

                self.previous_profile = self.current_profile;
                self.current_profile = idx;
                AppConfig::update(|c| c.active_profile_mut().audio_profile = idx);

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            if let Err(e) = set_easyeffects_profile(idx, None).await {
                                out.emit(AudioCommandOutput::Error(e));
                                return;
                            }
                            out.emit(AudioCommandOutput::ProfileSet(idx));
                        })
                        .drop_on_shutdown()
                });
            }

            SoundModesMsg::CustomPresetPathSelected(path) => {
                if extract_file_stem(&path).is_err() {
                    sender.input(SoundModesMsg::CustomCancelled(self.previous_profile));
                    return;
                }

                AppConfig::update(|c| {
                    c.active_profile_mut().audio_profile = CUSTOM_IDX;
                });

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match load_custom_preset(path).await {
                                Ok(n) => out.emit(AudioCommandOutput::CustomPresetLoaded(n)),
                                Err(e) => out.emit(AudioCommandOutput::Error(e)),
                            }
                        })
                        .drop_on_shutdown()
                });
            }

            SoundModesMsg::CustomCancelled(previous) => {
                self.current_profile = previous;
                self.dropdown.set_selected(previous);
                AppConfig::update(|c| c.active_profile_mut().audio_profile = previous);
            }
            SoundModesMsg::LoadProfile(idx) => {
                if !self.ee_installed {
                    return;
                }
                self.current_profile = idx;
                self.dropdown.set_selected(idx);
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            if let Err(e) = set_easyeffects_profile(idx, None).await {
                                out.emit(AudioCommandOutput::Error(e));
                                return;
                            }
                            out.emit(AudioCommandOutput::ProfileSet(idx));
                        })
                        .drop_on_shutdown()
                });
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: AudioCommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            AudioCommandOutput::EeChecked(installed) => {
                self.ee_installed = installed;
            }
            AudioCommandOutput::PresetsInstalled => {}
            AudioCommandOutput::ProfileSet(idx) => {
                tracing::info!("{}", t!("audio_profile_set", profile = idx));
            }
            AudioCommandOutput::CustomPresetLoaded(name) => {
                tracing::info!("{}", t!("audio_profile_set", profile = name));
            }
            AudioCommandOutput::Error(e) => {
                let _ = sender.output(e);
            }
        }
    }
}

fn parse_version_token(token: &str) -> Option<u32> {
    let clean = token.trim().trim_start_matches(['v', 'V']);
    let mut parts = clean.split('.');
    let major_str = parts.next()?;
    let minor_str = parts.next()?;
    if major_str.is_empty()
        || !major_str.chars().all(|c| c.is_ascii_digit())
        || !minor_str.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return None;
    }
    major_str.parse::<u32>().ok()
}

fn parse_easyeffects_major_version(output: &str) -> Option<u32> {
    for line in output.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(pos) = lower.find("easyeffects") {
            let after = &line[pos + "easyeffects".len()..];
            for token in after.split_whitespace() {
                if let Some(major) = parse_version_token(token) {
                    return Some(major);
                }
            }
        }
    }

    for token in output.split_whitespace() {
        if let Some(major) = parse_version_token(token) {
            return Some(major);
        }
    }

    None
}

fn resolve_easyeffects_output_dir(
    major_version: Option<u32>,
    xdg_data_home: Option<&Path>,
    xdg_config_home: Option<&Path>,
    home: &Path,
) -> PathBuf {
    if matches!(major_version, Some(v) if v >= 8) {
        let base = xdg_data_home
            .filter(|p| !p.as_os_str().is_empty() && p.is_absolute())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        base.join("easyeffects/output")
    } else {
        let base = xdg_config_home
            .filter(|p| !p.as_os_str().is_empty() && p.is_absolute())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        base.join("easyeffects/output")
    }
}

fn easyeffects_service_arg(major_version: Option<u32>) -> &'static str {
    if matches!(major_version, Some(v) if v >= 8) {
        "--service-mode"
    } else {
        "--gapplication-service"
    }
}

fn should_force_offscreen_qpa(
    wayland_display: Option<&std::ffi::OsStr>,
    display: Option<&std::ffi::OsStr>,
    qt_qpa_platform: Option<&std::ffi::OsStr>,
) -> bool {
    let has_display = wayland_display.is_some_and(|v| !v.is_empty())
        || display.is_some_and(|v| !v.is_empty());
    let has_qpa = qt_qpa_platform.is_some_and(|v| !v.is_empty());
    !has_display && !has_qpa
}

fn run_easyeffects_version_probe() -> std::io::Result<std::process::Output> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY");
    let display = std::env::var_os("DISPLAY");
    let qpa = std::env::var_os("QT_QPA_PLATFORM");
    let has_qpa = qpa.as_deref().is_some_and(|v| !v.is_empty());
    let force_offscreen =
        should_force_offscreen_qpa(wayland.as_deref(), display.as_deref(), qpa.as_deref());

    let mut cmd = std::process::Command::new("easyeffects");
    cmd.arg("--version");
    if force_offscreen {
        cmd.env("QT_QPA_PLATFORM", "offscreen");
    }
    let output = cmd.output()?;
    if !output.status.success() && !force_offscreen && !has_qpa {
        return std::process::Command::new("easyeffects")
            .arg("--version")
            .env("QT_QPA_PLATFORM", "offscreen")
            .output();
    }
    Ok(output)
}

async fn detect_easyeffects_major_version() -> Option<u32> {
    if let Some(&cached) = EASYEFFECTS_MAJOR_VERSION.get() {
        return Some(cached);
    }

    let output = tokio::task::spawn_blocking(run_easyeffects_version_probe)
        .await
        .ok()?
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let major = parse_easyeffects_major_version(&stdout)?;
    let _ = EASYEFFECTS_MAJOR_VERSION.set(major);
    Some(major)
}

async fn ensure_easyeffects_running() {
    let daemon_running = tokio::task::spawn_blocking(|| {
        std::process::Command::new("pgrep")
            .args(["-x", "easyeffects"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);

    if !daemon_running {
        let service_arg = easyeffects_service_arg(detect_easyeffects_major_version().await);
        let _ = tokio::process::Command::new("easyeffects")
            .arg(service_arg)
            .spawn();
        tokio::time::sleep(tokio::time::Duration::from_millis(
            EASYEFFECTS_STARTUP_DELAY_MS,
        ))
        .await;
    }
}

async fn easyeffects_output_dir() -> Result<PathBuf, String> {
    let major = detect_easyeffects_major_version().await;
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME environment variable is not set".to_string())?;
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    Ok(resolve_easyeffects_output_dir(
        major,
        xdg_data_home.as_deref(),
        xdg_config_home.as_deref(),
        &home,
    ))
}

fn extract_file_stem(path: &Path) -> Result<String, String> {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| "Invalid file name".to_string())
}

async fn set_easyeffects_profile(idx: u32, custom_name: Option<String>) -> Result<(), String> {
    if idx != NONE_IDX && idx != CUSTOM_IDX {
        install_presets().await?;
    }

    ensure_easyeffects_running().await;

    if idx == NONE_IDX {
        run_command_blocking("easyeffects", &["-b", "1"]).await?;
    } else if idx == CUSTOM_IDX {
        if let Some(name) = custom_name {
            run_command_blocking("easyeffects", &["-b", "2"]).await?;
            run_command_blocking("easyeffects", &["-l", &name]).await?;
        }
    } else {
        run_command_blocking("easyeffects", &["-b", "2"]).await?;
        let preset_idx = if idx < NONE_IDX { idx } else { idx - 1 } as usize;
        run_command_blocking("easyeffects", &["-l", PRESET_NAMES[preset_idx]]).await?;
    }

    Ok(())
}

async fn load_custom_preset(path: PathBuf) -> Result<String, String> {
    let name = extract_file_stem(&path)?;

    let dir = easyeffects_output_dir().await?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let dest = dir.join(format!("{name}.json"));
    tokio::fs::copy(&path, &dest)
        .await
        .map_err(|e| e.to_string())?;

    ensure_easyeffects_running().await;

    run_command_blocking("easyeffects", &["-b", "2"]).await?;
    run_command_blocking("easyeffects", &["-l", &name]).await?;

    Ok(name)
}

async fn install_presets() -> Result<(), String> {
    let dir = easyeffects_output_dir().await?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    for (name, content) in PRESETS {
        let path = dir.join(format!("{}.json", name));
        if !path.exists() {
            tokio::fs::write(&path, content)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_easyeffects_major_versions_case_insensitively() {
        assert_eq!(parse_easyeffects_major_version("easyeffects 8.2.8\n"), Some(8));
        assert_eq!(parse_easyeffects_major_version("EasyEffects 8.2.8"), Some(8));
        assert_eq!(parse_easyeffects_major_version("EASYEFFECTS 7.2.3\n"), Some(7));
        assert_eq!(parse_easyeffects_major_version("EasyEffects v7.1.6"), Some(7));
        assert_eq!(
            parse_easyeffects_major_version("easyeffects version 8.0.0-rc1"),
            Some(8)
        );
        assert_eq!(parse_easyeffects_major_version("8.2.8"), Some(8));
        assert_eq!(
            parse_easyeffects_major_version(
                "qt.qpa.plugin: From 6.5.0 warning\neasyeffects 8.2.8\n"
            ),
            Some(8)
        );
    }

    #[test]
    fn parse_easyeffects_major_version_rejects_invalid_output() {
        assert_eq!(parse_easyeffects_major_version(""), None);
        assert_eq!(parse_easyeffects_major_version("easyeffects"), None);
        assert_eq!(
            parse_easyeffects_major_version("easyeffects: command not found"),
            None
        );
        assert_eq!(parse_easyeffects_major_version("libgtk-4.so.1"), None);
    }

    #[test]
    fn resolves_output_dir_for_ee8_and_ee7() {
        let home = Path::new("/home/alice");

        assert_eq!(
            resolve_easyeffects_output_dir(Some(8), None, None, home),
            PathBuf::from("/home/alice/.local/share/easyeffects/output")
        );
        assert_eq!(
            resolve_easyeffects_output_dir(Some(9), Some(Path::new("/data/xdg")), None, home),
            PathBuf::from("/data/xdg/easyeffects/output")
        );
        assert_eq!(
            resolve_easyeffects_output_dir(Some(8), Some(Path::new("")), None, home),
            PathBuf::from("/home/alice/.local/share/easyeffects/output")
        );
        assert_eq!(
            resolve_easyeffects_output_dir(Some(8), Some(Path::new("relative/path")), None, home),
            PathBuf::from("/home/alice/.local/share/easyeffects/output")
        );

        assert_eq!(
            resolve_easyeffects_output_dir(Some(7), None, None, home),
            PathBuf::from("/home/alice/.config/easyeffects/output")
        );
        assert_eq!(
            resolve_easyeffects_output_dir(Some(7), None, Some(Path::new("/cfg/xdg")), home),
            PathBuf::from("/cfg/xdg/easyeffects/output")
        );
        assert_eq!(
            resolve_easyeffects_output_dir(Some(7), None, Some(Path::new("")), home),
            PathBuf::from("/home/alice/.config/easyeffects/output")
        );
        assert_eq!(
            resolve_easyeffects_output_dir(None, Some(Path::new("/data/xdg")), None, home),
            PathBuf::from("/home/alice/.config/easyeffects/output")
        );
    }

    #[test]
    fn selects_service_arg_by_major_version() {
        assert_eq!(easyeffects_service_arg(Some(8)), "--service-mode");
        assert_eq!(easyeffects_service_arg(Some(9)), "--service-mode");
        assert_eq!(easyeffects_service_arg(Some(7)), "--gapplication-service");
        assert_eq!(easyeffects_service_arg(None), "--gapplication-service");
    }

    #[test]
    fn forces_offscreen_qpa_only_when_display_and_qpa_are_unset() {
        use std::ffi::OsStr;

        assert!(should_force_offscreen_qpa(None, None, None));
        assert!(should_force_offscreen_qpa(
            Some(OsStr::new("")),
            Some(OsStr::new("")),
            Some(OsStr::new("")),
        ));
        assert!(!should_force_offscreen_qpa(
            Some(OsStr::new("wayland-0")),
            None,
            None,
        ));
        assert!(!should_force_offscreen_qpa(
            None,
            Some(OsStr::new(":0")),
            None,
        ));
        assert!(!should_force_offscreen_qpa(
            None,
            None,
            Some(OsStr::new("wayland")),
        ));
    }
}
