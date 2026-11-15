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

use std::time::Duration;

use gtk4 as gtk;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;

use crate::components::system::fan_osd;
use crate::services::config::AppConfig;
use crate::services::dbus;
use crate::services::dbus::FanProfile;
use crate::sys_paths::{CpuPowerTelemetry, read_cpu_power_telemetry};

/// Cycles Balanced → Performance → Quiet → Balanced. `LowPower` is treated as
/// `Quiet` for cycling purposes (TUF laptops); the daemon falls back internally.
fn next_profile(current: FanProfile) -> FanProfile {
    match current {
        FanProfile::Balanced => FanProfile::Performance,
        FanProfile::Performance => FanProfile::Quiet,
        FanProfile::Quiet | FanProfile::LowPower => FanProfile::Balanced,
    }
}

/// Formats read-only CPU EPP, governor, and Intel Turbo Boost telemetry for display.
/// Returns `None` when none of the sysfs telemetry files are readable on this host.
pub fn format_cpu_telemetry(tel: &CpuPowerTelemetry) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(epp) = tel.epp.as_deref() {
        parts.push(t!("fan_cpu_epp", epp = epp).to_string());
    }
    if let Some(gov) = tel.governor.as_deref() {
        parts.push(t!("fan_cpu_governor", governor = gov).to_string());
    }
    if let Some(turbo) = tel.turbo_enabled {
        let state = if turbo {
            t!("fan_cpu_turbo_on")
        } else {
            t!("fan_cpu_turbo_off")
        };
        parts.push(t!("fan_cpu_turbo", state = state).to_string());
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// State for the fan profile settings component.
pub struct FanModel {
    /// Whether the `asusd` daemon is reachable; disables all controls when `false`.
    asusd_available: bool,
    /// The currently active fan profile, used to suppress no-op toggle callbacks.
    current_profile: FanProfile,
    /// Read-only CPU EPP / governor / Turbo Boost summary.
    cpu_telemetry_subtitle: String,
    /// Only show the CPU telemetry row when at least one cpufreq/intel_pstate field exists.
    cpu_telemetry_available: bool,
    check_performance: gtk::CheckButton,
    check_balanced: gtk::CheckButton,
    check_quiet: gtk::CheckButton,
}

/// Input messages for the fan profile component.
#[derive(Debug)]
pub enum FanMsg {
    /// Switch to the given fan profile and persist the choice.
    ChangeProfile(FanProfile),
    /// Apply a profile without saving (called when switching hardware profiles).
    LoadProfile(FanProfile),
    /// Cycle to the next profile (triggered by the FN+F global hotkey).
    CycleFromHotkey,
    /// Refresh read-only CPU EPP / Turbo telemetry from sysfs.
    RefreshTelemetry,
}

/// Async command results for the fan profile component.
#[derive(Debug)]
pub enum FanCommandOutput {
    /// Result of the initial `asusd` availability check.
    AsusdChecked(bool),
    /// Initial profile read on startup — sync state without showing the OSD.
    InitialProfile(FanProfile),
    /// Confirmation that the profile was successfully applied.
    ProfileSet(FanProfile),
    /// Read-only CPU EPP / Turbo telemetry snapshot.
    CpuTelemetryRead(CpuPowerTelemetry),
    /// An error message to forward as a toast notification.
    Error(String),
}

#[relm4::component(pub)]
impl Component for FanModel {
    type Init = ();
    type Input = FanMsg;
    type Output = String;
    type CommandOutput = FanCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &t!("fan_group_title"),
            set_description: Some(&t!("fan_group_desc")),

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                #[watch]
                set_visible: !model.asusd_available,
                set_label: &t!("asusd_missing_warning"),
            },

            add = &adw::ActionRow {
                set_title: &t!("fan_performance_title"),
                set_subtitle: &t!("fan_performance_subtitle"),
                add_prefix = &model.check_performance.clone(),
                set_activatable_widget: Some(&model.check_performance),
                #[watch]
                set_sensitive: model.asusd_available,
            },

            add = &adw::ActionRow {
                set_title: &t!("fan_balanced_title"),
                set_subtitle: &t!("fan_balanced_subtitle"),
                add_prefix = &model.check_balanced.clone(),
                set_activatable_widget: Some(&model.check_balanced),
                #[watch]
                set_sensitive: model.asusd_available,
            },

            add = &adw::ActionRow {
                set_title: &t!("fan_quiet_title"),
                set_subtitle: &t!("fan_quiet_subtitle"),
                add_prefix = &model.check_quiet.clone(),
                set_activatable_widget: Some(&model.check_quiet),
                #[watch]
                set_sensitive: model.asusd_available,
            },

            add = &adw::ActionRow {
                set_title: &t!("fan_cpu_telemetry_title"),
                #[watch]
                set_subtitle: &gtk::glib::markup_escape_text(&model.cpu_telemetry_subtitle),
                #[watch]
                set_visible: model.cpu_telemetry_available,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let check_performance = gtk::CheckButton::new();
        let check_balanced = gtk::CheckButton::new();
        let check_quiet = gtk::CheckButton::new();

        check_balanced.set_group(Some(&check_performance));
        check_quiet.set_group(Some(&check_performance));

        let config = AppConfig::load();
        let saved_profile = FanProfile::from(config.active_profile().fan_profile);
        match saved_profile {
            FanProfile::Performance => check_performance.set_active(true),
            FanProfile::Balanced => check_balanced.set_active(true),
            FanProfile::Quiet | FanProfile::LowPower => check_quiet.set_active(true),
        }

        for (btn, profile) in [
            (&check_performance, FanProfile::Performance),
            (&check_balanced, FanProfile::Balanced),
            (&check_quiet, FanProfile::Quiet),
        ] {
            let sender = sender.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    sender.input(FanMsg::ChangeProfile(profile));
                }
            });
        }

        let model = FanModel {
            asusd_available: false,
            current_profile: saved_profile,
            cpu_telemetry_subtitle: String::new(),
            cpu_telemetry_available: false,
            check_performance,
            check_balanced,
            check_quiet,
        };

        let widgets = view_output!();

        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    if let Ok(tel) = tokio::task::spawn_blocking(read_cpu_power_telemetry).await {
                        out.emit(FanCommandOutput::CpuTelemetryRead(tel));
                    }
                })
                .drop_on_shutdown()
        });

        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    let available = dbus::check_asusd_available().await;
                    out.emit(FanCommandOutput::AsusdChecked(available));

                    if !available {
                        return;
                    }

                    match dbus::get_fan_profile().await {
                        Ok(current) if current == saved_profile => {
                            dbus::spawn_sync_power_profile(current);
                            out.emit(FanCommandOutput::InitialProfile(current));
                        }
                        Ok(_) => match dbus::set_fan_profile(saved_profile).await {
                            Ok(p) => out.emit(FanCommandOutput::ProfileSet(p)),
                            Err(e) => out.emit(FanCommandOutput::Error(e)),
                        },
                        Err(e) => out.emit(FanCommandOutput::Error(e)),
                    }
                })
                .drop_on_shutdown()
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: FanMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            FanMsg::ChangeProfile(profile) => {
                if profile == self.current_profile {
                    return;
                }
                self.current_profile = profile;
                AppConfig::update(|c| c.active_profile_mut().fan_profile = profile as u32);

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match dbus::set_fan_profile(profile).await {
                                Ok(p) => out.emit(FanCommandOutput::ProfileSet(p)),
                                Err(e) => out.emit(FanCommandOutput::Error(e)),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            FanMsg::CycleFromHotkey => {
                if !self.asusd_available {
                    return;
                }
                let target = next_profile(self.current_profile);
                self.current_profile = target;
                AppConfig::update(|c| c.active_profile_mut().fan_profile = target as u32);

                self.check_performance.set_active(target == FanProfile::Performance);
                self.check_balanced.set_active(target == FanProfile::Balanced);
                self.check_quiet
                    .set_active(target == FanProfile::Quiet || target == FanProfile::LowPower);

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match dbus::set_fan_profile(target).await {
                                Ok(p) => out.emit(FanCommandOutput::ProfileSet(p)),
                                Err(e) => out.emit(FanCommandOutput::Error(e)),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            FanMsg::LoadProfile(profile) => {
                if !self.asusd_available {
                    return;
                }
                self.current_profile = profile;
                self.check_performance.set_active(profile == FanProfile::Performance);
                self.check_balanced.set_active(profile == FanProfile::Balanced);
                self.check_quiet
                    .set_active(profile == FanProfile::Quiet || profile == FanProfile::LowPower);

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match dbus::set_fan_profile(profile).await {
                                Ok(p) => out.emit(FanCommandOutput::ProfileSet(p)),
                                Err(e) => out.emit(FanCommandOutput::Error(e)),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            FanMsg::RefreshTelemetry => {
                sender.command(|out, shutdown| {
                    shutdown
                        .register(async move {
                            if let Ok(tel) =
                                tokio::task::spawn_blocking(read_cpu_power_telemetry).await
                            {
                                out.emit(FanCommandOutput::CpuTelemetryRead(tel));
                            }
                        })
                        .drop_on_shutdown()
                });
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: FanCommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            FanCommandOutput::AsusdChecked(available) => {
                self.asusd_available = available;
            }
            FanCommandOutput::InitialProfile(profile) => {
                self.current_profile = profile;
                self.check_performance.set_active(profile == FanProfile::Performance);
                self.check_balanced.set_active(profile == FanProfile::Balanced);
                self.check_quiet
                    .set_active(profile == FanProfile::Quiet || profile == FanProfile::LowPower);
            }
            FanCommandOutput::ProfileSet(profile) => {
                let name = match profile {
                    FanProfile::Balanced => "Balanced",
                    FanProfile::Performance => "Performance",
                    FanProfile::Quiet => "Quiet",
                    FanProfile::LowPower => "LowPower",
                };
                tracing::info!("{}", t!("fan_profile_set", profile = name));
                self.current_profile = profile;
                fan_osd::show(profile, AppConfig::load().fan_osd_enabled);

                // power-profiles-daemon updates EPP slightly asynchronously after asusd switches profile.
                sender.command(|out, shutdown| {
                    shutdown
                        .register(async move {
                            tokio::time::sleep(Duration::from_millis(350)).await;
                            if let Ok(tel) =
                                tokio::task::spawn_blocking(read_cpu_power_telemetry).await
                            {
                                out.emit(FanCommandOutput::CpuTelemetryRead(tel));
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            FanCommandOutput::CpuTelemetryRead(tel) => {
                if let Some(summary) = format_cpu_telemetry(&tel) {
                    self.cpu_telemetry_subtitle = summary;
                    self.cpu_telemetry_available = true;
                } else {
                    self.cpu_telemetry_available = false;
                }
            }
            FanCommandOutput::Error(e) => {
                let _ = sender.output(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_cpu_telemetry_empty_and_populated() {
        let empty = CpuPowerTelemetry::default();
        assert_eq!(format_cpu_telemetry(&empty), None);

        let populated = CpuPowerTelemetry {
            epp: Some("balance_performance".to_string()),
            governor: Some("powersave".to_string()),
            turbo_enabled: Some(true),
        };
        let formatted = format_cpu_telemetry(&populated).expect("should format telemetry");
        assert!(formatted.contains("balance_performance"));
        assert!(formatted.contains("powersave"));
        assert!(formatted.contains(&t!("fan_cpu_turbo_on").to_string()));
    }
}
