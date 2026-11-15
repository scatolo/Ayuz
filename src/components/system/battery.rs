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

use std::cell::Cell;
use std::rc::Rc;

use gtk4::glib;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;

use crate::services::commands::pkexec_write_sysfs;
use crate::services::config::AppConfig;
use crate::services::dbus;
use crate::sys_paths::{BatteryTelemetry, SYS_MEM_SLEEP, read_battery_telemetry};

const DEFAULT_LIMIT_PRESETS: [u8; 3] = [60, 80, 100];

/// Builds sorted, deduplicated charge-limit presets (`60`, `80`, `100` plus an optional firmware value).
pub fn build_limit_presets(init_val: Option<u8>) -> Vec<u8> {
    let mut presets = DEFAULT_LIMIT_PRESETS.to_vec();
    if let Some(v) = init_val
        && (20..=100).contains(&v)
        && !presets.contains(&v)
    {
        presets.push(v);
    }
    presets.sort_unstable();
    presets.dedup();
    presets
}

/// Formats battery health, cycle count, status, and live power draw into a concise summary string.
pub fn format_battery_summary(tel: Option<&BatteryTelemetry>) -> String {
    let Some(tel) = tel else {
        return t!("battery_status_unavailable").to_string();
    };

    let mut parts = Vec::new();
    match (tel.capacity_percent, tel.status.as_deref()) {
        (Some(cap), Some(st)) => parts.push(format!("{cap}% ({st})")),
        (Some(cap), None) => parts.push(format!("{cap}%")),
        (None, Some(st)) => parts.push(st.to_string()),
        (None, None) => {}
    }

    if let Some(watts) = tel.power_watts {
        parts.push(format!("{watts:.1} W"));
    }

    if let Some(health) = tel.health_percent {
        parts.push(t!("battery_status_health", health = format!("{health:.0}%")).to_string());
    }

    if let Some(cycles) = tel.cycle_count {
        parts.push(t!("battery_status_cycles", cycles = cycles.to_string()).to_string());
    }

    if parts.is_empty() {
        t!("battery_status_unavailable").to_string()
    } else {
        parts.join(" · ")
    }
}

/// State for the battery settings component.
pub struct BatteryModel {
    /// Whether the `asusd` daemon is reachable; disables charge controls when `false`.
    asusd_available: bool,
    /// Available charge-limit presets shown in the `ComboRow` (typically `60`, `80`, `100`).
    limit_presets: Vec<u8>,
    /// Currently selected charge-limit preset in `%` (`60`, `80`, `100`, or firmware custom value).
    charge_limit: u8,
    /// Last non-100% charge limit to restore when temporary 24-hour full charge turns off or expires.
    saved_limit: u8,
    /// When `true`, the charge limit is temporarily raised to 100% for up to 24 hours.
    full_charge_active: bool,
    /// When `true`, suspend-to-RAM uses the `deep` sleep state instead of `s2idle`.
    deep_sleep_active: bool,
    /// When `true` the `deep_sleep` is supported by device.
    deep_sleep_supported: bool,
    /// Human-readable battery telemetry summary (capacity, status, wattage, health, cycles).
    telemetry_summary: String,
    /// ComboRow for selecting charge limit preset.
    limit_combo: adw::ComboRow,
    /// Suppresses `connect_selected_notify` while programmatically rebuilding/syncing `limit_combo`.
    suppress_signals: Rc<Cell<bool>>,
    /// Cancels the 24-hour full-charge revert timer when the user turns off full-charge mode early.
    timer_cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BatteryModel {
    fn sync_limit_combo(&self) {
        let labels: Vec<String> = self.limit_presets.iter().map(|v| format!("{v}%")).collect();
        let str_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected_idx = self
            .limit_presets
            .iter()
            .position(|&v| v == self.charge_limit)
            .unwrap_or(1) as u32;

        self.suppress_signals.set(true);
        self.limit_combo
            .set_model(Some(&gtk4::StringList::new(&str_refs)));
        self.limit_combo.set_selected(selected_idx);
        self.suppress_signals.set(false);
    }
}

/// Input messages for the battery component.
#[derive(Debug)]
pub enum BatteryOutput {
    Error(String),
    ChargeLimitChanged {
        limit: u8,
        asusd_available: bool,
    },
}

#[derive(Debug)]
pub enum BatteryMsg {
    /// Select a charge-limit preset index from `limit_combo`.
    SelectChargeLimit(u32),
    /// Enable or disable the temporary 100% full-charge mode (auto-reverts after 24 h).
    ToggleFullCharge(bool),
    /// Toggle between 80% Battery Maintenance (`<= 80`) and 100% full capacity from the tray.
    ToggleMaintenanceFromTray,
    /// Switch suspend-to-RAM between `s2idle` (`false`) and `deep` (`true`).
    ToggleDeepSleep(bool),
    /// Apply deep-sleep setting from a profile without saving.
    LoadProfile(bool),
    /// Refresh battery health, cycle count, and live power draw from sysfs.
    RefreshStats,
}

/// Async command results for the battery component.
#[derive(Debug)]
pub enum BatteryCommandOutput {
    /// Result of the initial `asusd` availability check.
    AsusdChecked(bool),
    /// Confirmation that the charge limit was successfully written.
    ChargeLimitSet(u8),
    /// Failed charge-limit write, carrying UI rollback state.
    ChargeLimitError {
        error: String,
        revert_limit: u8,
        revert_saved_limit: u8,
        revert_full_charge: bool,
    },
    /// An error message to forward as a toast notification.
    Error(String),
    /// Fired after the 24-hour full-charge timer expires, triggering a revert to `saved_limit`.
    TimerElapsed,
    /// Charge limit value read from `asusd` during initialisation.
    InitValue(u8),
    /// Deep-sleep state read from `/sys/power/mem_sleep` during initialisation.
    InitDeepSleep(bool),
    /// Confirmation that the `/sys/power/mem_sleep` write succeeded.
    DeepSleepSet(bool),
    /// Whether deep_sleep is supported by the device.
    DeepSleepSupported(bool),
    /// Refreshed battery telemetry from `/sys/class/power_supply/*`.
    TelemetryRead(Option<BatteryTelemetry>),
}

#[relm4::component(pub)]
impl Component for BatteryModel {
    type Init = ();
    type Input = BatteryMsg;
    type Output = BatteryOutput;
    type CommandOutput = BatteryCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &glib::markup_escape_text(&t!("battery_group_title")),
            set_description: Some(&t!("battery_group_desc")),

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                #[watch]
                set_visible: !model.asusd_available,
                set_label: &t!("asusd_missing_warning"),
            },

            add = &adw::ActionRow {
                set_title: &t!("battery_status_title"),
                #[watch]
                set_subtitle: &glib::markup_escape_text(&model.telemetry_summary),

                add_suffix = &gtk4::Button {
                    set_icon_name: "view-refresh-symbolic",
                    add_css_class: "flat",
                    set_valign: gtk4::Align::Center,
                    set_tooltip_text: Some(&t!("battery_refresh_tooltip")),
                    connect_clicked => BatteryMsg::RefreshStats,
                },
            },

            add = &model.limit_combo.clone() -> adw::ComboRow {
                set_title: &t!("battery_limit_title"),
                set_subtitle: &t!("battery_limit_subtitle"),

                #[watch]
                set_sensitive: model.asusd_available && !model.full_charge_active,
            },

            add = &adw::SwitchRow {
                set_title: &t!("battery_full_charge_title"),
                set_subtitle: &t!("battery_full_charge_subtitle"),

                #[watch]
                set_active: model.full_charge_active,

                #[watch]
                set_sensitive: model.asusd_available && (model.charge_limit < 100 || model.full_charge_active),

                connect_active_notify[sender] => move |switch| {
                    sender.input(BatteryMsg::ToggleFullCharge(switch.is_active()));
                },
            },

            add = &adw::SwitchRow {
                set_title: &t!("battery_deep_sleep_title"),
                #[watch]
                set_subtitle: &if model.deep_sleep_supported {
                    t!("battery_deep_sleep_subtitle")
                } else {
                    t!("battery_deep_sleep_not_supported")
                },
                #[watch]
                set_sensitive: model.deep_sleep_supported,
                #[watch]
                set_active: model.deep_sleep_active,

                connect_active_notify[sender] => move |switch| {
                    sender.input(BatteryMsg::ToggleDeepSleep(switch.is_active()));
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let limit_combo = adw::ComboRow::new();
        let suppress_signals = Rc::new(Cell::new(false));
        {
            let sender = sender.clone();
            let suppress = suppress_signals.clone();
            limit_combo.connect_selected_notify(move |row| {
                if suppress.get() {
                    return;
                }
                sender.input(BatteryMsg::SelectChargeLimit(row.selected()));
            });
        }

        let model = BatteryModel {
            asusd_available: false,
            limit_presets: build_limit_presets(None),
            charge_limit: 80,
            saved_limit: 80,
            full_charge_active: false,
            deep_sleep_active: false,
            deep_sleep_supported: false,
            telemetry_summary: t!("battery_status_loading").to_string(),
            limit_combo,
            suppress_signals,
            timer_cancel: None,
        };
        model.sync_limit_combo();
        let widgets = view_output!();

        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    let available = dbus::check_asusd_available().await;
                    out.emit(BatteryCommandOutput::AsusdChecked(available));

                    if !available {
                        return;
                    }

                    match dbus::get_charge_limit().await {
                        Ok(val) => out.emit(BatteryCommandOutput::InitValue(val)),
                        Err(e) => out.emit(BatteryCommandOutput::Error(e)),
                    }
                })
                .drop_on_shutdown()
        });

        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    match tokio::fs::read_to_string(SYS_MEM_SLEEP).await {
                        Ok(content) => {
                            let active = content.contains("[deep]");
                            let supported = content.contains("deep");

                            out.emit(BatteryCommandOutput::InitDeepSleep(active));
                            out.emit(BatteryCommandOutput::DeepSleepSupported(supported));
                        }
                        Err(e) => {
                            out.emit(BatteryCommandOutput::Error(
                                t!("error_mem_sleep_read", error = e.to_string()).to_string(),
                            ));
                        }
                    }
                })
                .drop_on_shutdown()
        });

        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    let tel = tokio::task::spawn_blocking(read_battery_telemetry)
                        .await
                        .ok()
                        .flatten();
                    out.emit(BatteryCommandOutput::TelemetryRead(tel));
                })
                .drop_on_shutdown()
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: BatteryMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            BatteryMsg::SelectChargeLimit(idx) => {
                let Some(&new_limit) = self.limit_presets.get(idx as usize) else {
                    return;
                };
                if new_limit == self.charge_limit && !self.full_charge_active {
                    return;
                }

                let prev_limit = self.charge_limit;
                let prev_saved = self.saved_limit;
                let prev_full = self.full_charge_active;

                self.full_charge_active = false;
                if let Some(cancel) = self.timer_cancel.take() {
                    let _ = cancel.send(());
                }

                self.charge_limit = new_limit;
                if new_limit < 100 {
                    self.saved_limit = new_limit;
                }

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match dbus::set_charge_limit(new_limit).await {
                                Ok(val) => out.emit(BatteryCommandOutput::ChargeLimitSet(val)),
                                Err(error) => out.emit(BatteryCommandOutput::ChargeLimitError {
                                    error,
                                    revert_limit: prev_limit,
                                    revert_saved_limit: prev_saved,
                                    revert_full_charge: prev_full,
                                }),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            BatteryMsg::ToggleDeepSleep(active) => {
                if active && !self.deep_sleep_supported {
                    let _ = sender.output(BatteryOutput::Error(
                        t!("battery_deep_sleep_not_supported").to_string(),
                    ));
                    return;
                }
                if active == self.deep_sleep_active {
                    return;
                }

                self.deep_sleep_active = active;
                AppConfig::update(|c| c.active_profile_mut().battery_deep_sleep_active = active);

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let value = if active { "deep" } else { "s2idle" };
                            match pkexec_write_sysfs(SYS_MEM_SLEEP, value).await {
                                Ok(()) => out.emit(BatteryCommandOutput::DeepSleepSet(active)),
                                Err(e) => out.emit(BatteryCommandOutput::Error(e)),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            BatteryMsg::LoadProfile(active) => {
                if self.deep_sleep_active == active || !self.deep_sleep_supported {
                    return;
                }
                self.deep_sleep_active = active;
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            let value = if active { "deep" } else { "s2idle" };
                            match pkexec_write_sysfs(SYS_MEM_SLEEP, value).await {
                                Ok(()) => out.emit(BatteryCommandOutput::DeepSleepSet(active)),
                                Err(e) => out.emit(BatteryCommandOutput::Error(e)),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            BatteryMsg::ToggleFullCharge(active) => {
                if active == self.full_charge_active {
                    return;
                }
                if active && self.charge_limit >= 100 {
                    return;
                }

                let prev_limit = self.charge_limit;
                let prev_saved = self.saved_limit;
                let prev_full = self.full_charge_active;
                self.full_charge_active = active;

                if let Some(cancel) = self.timer_cancel.take() {
                    let _ = cancel.send(());
                }

                if active {
                    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                    self.timer_cancel = Some(tx);

                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match dbus::set_charge_limit(100).await {
                                    Ok(val) => {
                                        out.emit(BatteryCommandOutput::ChargeLimitSet(val));
                                        tokio::select! {
                                            _ = tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)) => {
                                                out.emit(BatteryCommandOutput::TimerElapsed);
                                            }
                                            _ = rx => {}
                                        }
                                    }
                                    Err(error) => {
                                        out.emit(BatteryCommandOutput::ChargeLimitError {
                                            error,
                                            revert_limit: prev_limit,
                                            revert_saved_limit: prev_saved,
                                            revert_full_charge: prev_full,
                                        });
                                    }
                                }
                            })
                            .drop_on_shutdown()
                    });
                } else {
                    let restore = self.saved_limit;
                    self.charge_limit = restore;
                    self.sync_limit_combo();
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match dbus::set_charge_limit(restore).await {
                                    Ok(val) => out.emit(BatteryCommandOutput::ChargeLimitSet(val)),
                                    Err(error) => out.emit(BatteryCommandOutput::ChargeLimitError {
                                        error,
                                        revert_limit: prev_limit,
                                        revert_saved_limit: prev_saved,
                                        revert_full_charge: prev_full,
                                    }),
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            BatteryMsg::ToggleMaintenanceFromTray => {
                if !self.asusd_available {
                    return;
                }
                let target = if self.charge_limit <= 80 { 100 } else { 80 };
                let prev_limit = self.charge_limit;
                let prev_saved = self.saved_limit;
                let prev_full = self.full_charge_active;

                if let Some(cancel) = self.timer_cancel.take() {
                    let _ = cancel.send(());
                }

                self.full_charge_active = false;
                self.charge_limit = target;
                if target < 100 {
                    self.saved_limit = target;
                }
                self.limit_presets = build_limit_presets(Some(target));
                self.sync_limit_combo();
                let _ = sender.output(BatteryOutput::ChargeLimitChanged {
                    limit: target,
                    asusd_available: self.asusd_available,
                });

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match dbus::set_charge_limit(target).await {
                                Ok(val) => out.emit(BatteryCommandOutput::ChargeLimitSet(val)),
                                Err(error) => out.emit(BatteryCommandOutput::ChargeLimitError {
                                    error,
                                    revert_limit: prev_limit,
                                    revert_saved_limit: prev_saved,
                                    revert_full_charge: prev_full,
                                }),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            BatteryMsg::RefreshStats => {
                sender.command(|out, shutdown| {
                    shutdown
                        .register(async move {
                            let tel = tokio::task::spawn_blocking(read_battery_telemetry)
                                .await
                                .ok()
                                .flatten();
                            out.emit(BatteryCommandOutput::TelemetryRead(tel));
                        })
                        .drop_on_shutdown()
                });
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: BatteryCommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            BatteryCommandOutput::AsusdChecked(available) => {
                self.asusd_available = available;
                let _ = sender.output(BatteryOutput::ChargeLimitChanged {
                    limit: self.charge_limit,
                    asusd_available: available,
                });
            }
            BatteryCommandOutput::InitValue(val) => {
                self.limit_presets = build_limit_presets(Some(val));
                self.charge_limit = val;
                if val < 100 {
                    self.saved_limit = val;
                }
                self.full_charge_active = false;
                self.sync_limit_combo();
                let _ = sender.output(BatteryOutput::ChargeLimitChanged {
                    limit: val,
                    asusd_available: self.asusd_available,
                });
            }
            BatteryCommandOutput::InitDeepSleep(active) => {
                self.deep_sleep_active = active;
            }
            BatteryCommandOutput::DeepSleepSupported(supported) => {
                self.deep_sleep_supported = supported;

                if !supported {
                    self.deep_sleep_active = false;
                }
            }
            BatteryCommandOutput::DeepSleepSet(active) => {
                let value = if active && self.deep_sleep_supported {
                    "deep"
                } else {
                    "s2idle"
                };
                tracing::info!("{}", t!("battery_deep_sleep_set", value = value));
            }
            BatteryCommandOutput::ChargeLimitSet(val) => {
                self.charge_limit = val;
                let _ = sender.output(BatteryOutput::ChargeLimitChanged {
                    limit: val,
                    asusd_available: self.asusd_available,
                });
                tracing::info!(
                    "{}",
                    t!("battery_charge_limit_set", value = val.to_string())
                );
            }
            BatteryCommandOutput::ChargeLimitError {
                error,
                revert_limit,
                revert_saved_limit,
                revert_full_charge,
            } => {
                if let Some(cancel) = self.timer_cancel.take() {
                    let _ = cancel.send(());
                }
                self.charge_limit = revert_limit;
                self.saved_limit = revert_saved_limit;
                self.full_charge_active = revert_full_charge;
                self.sync_limit_combo();
                let _ = sender.output(BatteryOutput::ChargeLimitChanged {
                    limit: revert_limit,
                    asusd_available: self.asusd_available,
                });
                let _ = sender.output(BatteryOutput::Error(error));
            }
            BatteryCommandOutput::Error(e) => {
                let _ = sender.output(BatteryOutput::Error(e));
            }
            BatteryCommandOutput::TimerElapsed => {
                let prev_limit = self.charge_limit;
                let prev_saved = self.saved_limit;
                self.full_charge_active = false;
                self.timer_cancel = None;
                let restore = self.saved_limit;
                self.charge_limit = restore;
                self.sync_limit_combo();
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match dbus::set_charge_limit(restore).await {
                                Ok(val) => out.emit(BatteryCommandOutput::ChargeLimitSet(val)),
                                Err(error) => out.emit(BatteryCommandOutput::ChargeLimitError {
                                    error,
                                    revert_limit: prev_limit,
                                    revert_saved_limit: prev_saved,
                                    revert_full_charge: true,
                                }),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            BatteryCommandOutput::TelemetryRead(tel) => {
                self.telemetry_summary = format_battery_summary(tel.as_ref());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_limit_presets_default_and_custom() {
        assert_eq!(build_limit_presets(None), vec![60, 80, 100]);
        assert_eq!(build_limit_presets(Some(80)), vec![60, 80, 100]);
        assert_eq!(build_limit_presets(Some(75)), vec![60, 75, 80, 100]);
        assert_eq!(build_limit_presets(Some(50)), vec![50, 60, 80, 100]);
        assert_eq!(build_limit_presets(Some(10)), vec![60, 80, 100]);
    }

    #[test]
    fn test_format_battery_summary_full_and_missing() {
        assert_eq!(
            format_battery_summary(None),
            t!("battery_status_unavailable").to_string()
        );

        let tel = BatteryTelemetry {
            capacity_percent: Some(79),
            status: Some("Not charging".to_string()),
            power_watts: Some(8.4),
            health_percent: Some(96.4),
            cycle_count: Some(42),
        };
        let summary = format_battery_summary(Some(&tel));
        assert!(summary.contains("79% (Not charging)"));
        assert!(summary.contains("8.4 W"));
        assert!(summary.contains("96%"));
        assert!(summary.contains("42"));
    }
}
