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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gtk4 as gtk;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;

use crate::components::system::fan_osd;
use crate::services::config::AppConfig;
use crate::services::cpu_tuning::{
    self, ApplyMaxPerfOutcome, build_max_perf_presets, format_max_perf_preset_label,
};
use crate::services::dbus;
use crate::services::dbus::FanProfile;
use crate::sys_paths::{
    CpuPowerTelemetry, FanRpmTelemetry, clamp_perf_pct, format_fan_rpm_summary,
    read_cpu_power_telemetry, read_fan_rpm_telemetry,
};

static MAX_PERF_APPLY_SEQ: AtomicU64 = AtomicU64::new(0);

/// Cycles Balanced → Performance → Quiet → Balanced. `LowPower` is treated as
/// `Quiet` for cycling purposes (TUF laptops); the daemon falls back internally.
fn next_profile(current: FanProfile) -> FanProfile {
    match current {
        FanProfile::Balanced => FanProfile::Performance,
        FanProfile::Performance => FanProfile::Quiet,
        FanProfile::Quiet | FanProfile::LowPower => FanProfile::Balanced,
    }
}

/// Formats read-only CPU EPP, governor, Intel Turbo Boost, and `intel_pstate` perf limits for display.
/// Returns `None` when none of the sysfs telemetry fields are readable on this host.
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
    if let Some(max_pct) = tel.max_perf_pct {
        parts.push(t!("fan_cpu_max_perf", pct = max_pct.to_string()).to_string());
    }
    if let Some(min_pct) = tel.min_perf_pct
        && min_pct >= 95
    {
        parts.push(t!("fan_cpu_min_perf_high", pct = min_pct.to_string()).to_string());
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Formats fan RPM telemetry for display in System -> Fan Mode.
pub fn format_fan_rpm_status(tel: &FanRpmTelemetry) -> String {
    if tel.fans.is_empty() {
        return t!("fan_rpm_unavailable").to_string();
    }
    if tel.fans.iter().all(|f| f.rpm == 0) {
        return tel
            .fans
            .iter()
            .map(|f| format!("{}: {}", f.label, t!("fan_rpm_off")))
            .collect::<Vec<_>>()
            .join(" · ");
    }
    format_fan_rpm_summary(tel).unwrap_or_else(|| t!("fan_rpm_unavailable").to_string())
}

/// State for the fan profile settings component.
pub struct FanModel {
    /// Whether the `asusd` daemon is reachable; disables fan profile controls when `false`.
    asusd_available: bool,
    /// The currently active fan profile, used to suppress no-op toggle callbacks.
    current_profile: FanProfile,
    /// Read-only live fan RPM summary.
    fan_rpm_subtitle: String,
    /// Watch channel controlling whether the 3-second live RPM polling loop is active.
    fan_polling_tx: tokio::sync::watch::Sender<bool>,
    /// Read-only CPU EPP / governor / Turbo Boost / `max_perf_pct` summary.
    cpu_telemetry_subtitle: String,
    /// Only show the CPU telemetry row when at least one cpufreq/intel_pstate field exists.
    cpu_telemetry_available: bool,
    /// Whether `/sys/devices/system/cpu/intel_pstate/max_perf_pct` exists on this CPU.
    max_perf_supported: bool,
    /// Whether `max_perf_pct` is writable without `pkexec` (via `systemd-tmpfiles` + `input`).
    pstate_writable: bool,
    /// Selected per-profile `max_perf_pct` ceiling (`None` = system default / 100%).
    cpu_max_perf_pct: Option<u8>,
    /// Available dropdown presets (`[None, Some(60), ..., Some(100)]`).
    max_perf_presets: Vec<Option<u8>>,
    max_perf_combo: adw::ComboRow,
    suppress_signals: Rc<Cell<bool>>,
    check_performance: gtk::CheckButton,
    check_balanced: gtk::CheckButton,
    check_quiet: gtk::CheckButton,
}

/// Input messages for the fan profile component.
#[derive(Debug)]
pub enum FanMsg {
    /// Switch to the given fan profile and persist the choice.
    ChangeProfile(FanProfile),
    /// Apply fan profile and CPU max-performance ceiling from a hardware profile without saving.
    LoadProfile {
        profile: FanProfile,
        cpu_max_perf_pct: Option<u8>,
    },
    /// Select a CPU `max_perf_pct` preset index from `max_perf_combo`.
    SelectMaxPerf(u32),
    /// Cycle to the next profile (triggered by the FN+F global hotkey).
    CycleFromHotkey,
    /// Apply a fan profile selected from the system tray menu.
    SetFromTray(FanProfile),
    /// Refresh read-only CPU EPP / Turbo / `intel_pstate` and fan RPM telemetry from sysfs.
    RefreshTelemetry,
    /// Enable or pause the 3-second live fan RPM polling loop when System page visibility changes.
    SetPollingActive(bool),
}

/// Output events emitted by [`FanModel`] to the parent [`AppModel`].
#[derive(Debug)]
pub enum FanOutput {
    Error(String),
    StateChanged {
        profile: FanProfile,
        asusd_available: bool,
    },
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
    /// External change observed via `xyz.ljones.Platform` or `UPower.PowerProfiles`.
    ExternalProfileChanged {
        source: dbus::ExternalProfileSource,
        profile: FanProfile,
    },
    /// Result of applying `intel_pstate/max_perf_pct`.
    MaxPerfApplied {
        outcome: ApplyMaxPerfOutcome,
        is_profile_load: bool,
    },
    /// Read-only CPU EPP / Turbo telemetry snapshot.
    CpuTelemetryRead(CpuPowerTelemetry),
    /// Live fan RPM telemetry snapshot from hwmon.
    FanRpmRead(FanRpmTelemetry),
    /// An error message to forward as a toast notification.
    Error(String),
}

impl FanModel {
    fn sync_radio_buttons(&self, profile: FanProfile) {
        self.check_performance
            .set_active(profile == FanProfile::Performance);
        self.check_balanced
            .set_active(profile == FanProfile::Balanced);
        self.check_quiet
            .set_active(profile == FanProfile::Quiet || profile == FanProfile::LowPower);
    }

    fn sync_max_perf_combo(&self) {
        let labels: Vec<String> = self
            .max_perf_presets
            .iter()
            .map(|&p| format_max_perf_preset_label(p))
            .collect();
        let str_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let normalized = self.cpu_max_perf_pct.map(clamp_perf_pct);
        let selected_idx = self
            .max_perf_presets
            .iter()
            .position(|&p| p == normalized)
            .unwrap_or(0) as u32;

        self.suppress_signals.set(true);
        self.max_perf_combo
            .set_model(Some(&gtk::StringList::new(&str_refs)));
        self.max_perf_combo.set_selected(selected_idx);
        self.suppress_signals.set(false);
    }

    fn emit_state(&self, sender: &ComponentSender<Self>) {
        let _ = sender.output(FanOutput::StateChanged {
            profile: self.current_profile,
            asusd_available: self.asusd_available,
        });
    }

    fn spawn_refresh_cpu_telemetry(&self, sender: &ComponentSender<Self>) {
        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    // Wait for tuned-ppd / power-profiles-daemon to finish applying EPP/min_perf_pct.
                    tokio::time::sleep(Duration::from_millis(450)).await;
                    if let Ok(tel) =
                        tokio::task::spawn_blocking(read_cpu_power_telemetry).await
                    {
                        out.emit(FanCommandOutput::CpuTelemetryRead(tel));
                    }
                    if let Ok(rpm) =
                        tokio::task::spawn_blocking(read_fan_rpm_telemetry).await
                    {
                        out.emit(FanCommandOutput::FanRpmRead(rpm));
                    }
                })
                .drop_on_shutdown()
        });
    }

    fn spawn_apply_max_perf(
        &self,
        configured: Option<u8>,
        prev_configured: Option<u8>,
        delay_ms: u64,
        is_profile_load: bool,
        sender: &ComponentSender<Self>,
    ) {
        let seq = MAX_PERF_APPLY_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    if delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                    if MAX_PERF_APPLY_SEQ.load(Ordering::SeqCst) != seq {
                        return;
                    }
                    match cpu_tuning::apply_max_perf_pct(configured, prev_configured).await {
                        Ok(outcome) => {
                            if MAX_PERF_APPLY_SEQ.load(Ordering::SeqCst) != seq {
                                return;
                            }
                            out.emit(FanCommandOutput::MaxPerfApplied {
                                outcome,
                                is_profile_load,
                            });
                        }
                        Err(e) => out.emit(FanCommandOutput::Error(e)),
                    }
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

#[relm4::component(pub)]
impl Component for FanModel {
    type Init = ();
    type Input = FanMsg;
    type Output = FanOutput;
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
                set_title: &t!("fan_rpm_title"),
                #[watch]
                set_subtitle: &gtk::glib::markup_escape_text(&model.fan_rpm_subtitle),

                add_suffix = &gtk::Button {
                    set_icon_name: "view-refresh-symbolic",
                    add_css_class: "flat",
                    set_valign: gtk::Align::Center,
                    set_tooltip_text: Some(&t!("fan_rpm_refresh_tooltip")),
                    connect_clicked => FanMsg::RefreshTelemetry,
                },
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

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                #[watch]
                set_visible: model.max_perf_supported && !model.pstate_writable,
                set_label: &t!("fan_cpu_max_perf_permission_warning"),
            },

            add = &model.max_perf_combo.clone() -> adw::ComboRow {
                set_title: &t!("fan_cpu_max_perf_title"),
                set_subtitle: &t!("fan_cpu_max_perf_subtitle"),
                #[watch]
                set_visible: model.max_perf_supported,
                #[watch]
                set_sensitive: model.max_perf_supported && model.pstate_writable,
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
        let active_profile = config.active_profile();
        let saved_profile = FanProfile::from(active_profile.fan_profile);
        let saved_max_perf = active_profile.cpu_max_perf_pct.map(clamp_perf_pct);
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

        let max_perf_combo = adw::ComboRow::new();
        let suppress_signals = Rc::new(Cell::new(false));
        {
            let sender = sender.clone();
            let suppress = suppress_signals.clone();
            max_perf_combo.connect_selected_notify(move |row| {
                if suppress.get() {
                    return;
                }
                sender.input(FanMsg::SelectMaxPerf(row.selected()));
            });
        }

        let (fan_polling_tx, mut fan_polling_rx) = tokio::sync::watch::channel(false);

        let model = FanModel {
            asusd_available: false,
            current_profile: saved_profile,
            fan_rpm_subtitle: "…".to_string(),
            fan_polling_tx,
            cpu_telemetry_subtitle: String::new(),
            cpu_telemetry_available: false,
            max_perf_supported: false,
            pstate_writable: false,
            cpu_max_perf_pct: saved_max_perf,
            max_perf_presets: build_max_perf_presets(saved_max_perf),
            max_perf_combo,
            suppress_signals,
            check_performance,
            check_balanced,
            check_quiet,
        };
        model.sync_max_perf_combo();

        let widgets = view_output!();

        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    if let Ok(tel) = tokio::task::spawn_blocking(read_cpu_power_telemetry).await {
                        out.emit(FanCommandOutput::CpuTelemetryRead(tel));
                    }
                    if let Ok(rpm) = tokio::task::spawn_blocking(read_fan_rpm_telemetry).await {
                        out.emit(FanCommandOutput::FanRpmRead(rpm));
                    }
                })
                .drop_on_shutdown()
        });

        // Poll live fan RPM every 3 seconds while the System page is visible.
        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    loop {
                        while !*fan_polling_rx.borrow_and_update() {
                            if fan_polling_rx.changed().await.is_err() {
                                return;
                            }
                        }

                        if let Ok(rpm) = tokio::task::spawn_blocking(read_fan_rpm_telemetry).await {
                            out.emit(FanCommandOutput::FanRpmRead(rpm));
                        }

                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                            res = fan_polling_rx.changed() => {
                                if res.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                })
                .drop_on_shutdown()
        });

        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    let startup_seq = MAX_PERF_APPLY_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
                    let available = dbus::check_asusd_available().await;
                    out.emit(FanCommandOutput::AsusdChecked(available));

                    if !available {
                        let startup_max_perf = AppConfig::load()
                            .active_profile()
                            .cpu_max_perf_pct
                            .map(clamp_perf_pct);
                        if startup_max_perf.is_some()
                            && MAX_PERF_APPLY_SEQ.load(Ordering::SeqCst) == startup_seq
                            && let Ok(outcome) =
                                cpu_tuning::apply_max_perf_pct(startup_max_perf, None).await
                        {
                            out.emit(FanCommandOutput::MaxPerfApplied {
                                outcome,
                                is_profile_load: true,
                            });
                            if let Ok(tel) =
                                tokio::task::spawn_blocking(read_cpu_power_telemetry).await
                            {
                                out.emit(FanCommandOutput::CpuTelemetryRead(tel));
                            }
                        }
                        return;
                    }

                    // Re-read active profile after the async D-Bus probe in case
                    // `power_auto` activated a different profile during startup.
                    let active = AppConfig::load().active_profile().clone();
                    let saved_profile = FanProfile::from(active.fan_profile);
                    let startup_max_perf = active.cpu_max_perf_pct.map(clamp_perf_pct);

                    match dbus::get_fan_profile().await {
                        Ok(current)
                            if dbus::is_equivalent_fan_profile(current, saved_profile) =>
                        {
                            dbus::sync_power_profile_now(current).await;
                            out.emit(FanCommandOutput::InitialProfile(current));
                        }
                        Ok(_) => match dbus::set_platform_profile_only(saved_profile).await {
                            Ok(p) => {
                                dbus::sync_power_profile_now(p).await;
                                out.emit(FanCommandOutput::ProfileSet(p));
                            }
                            Err(e) => {
                                out.emit(FanCommandOutput::Error(e));
                                return;
                            }
                        },
                        Err(e) => {
                            out.emit(FanCommandOutput::Error(e));
                            return;
                        }
                    }

                    if startup_max_perf.is_some() {
                        tokio::time::sleep(Duration::from_millis(450)).await;
                        if MAX_PERF_APPLY_SEQ.load(Ordering::SeqCst) == startup_seq
                            && let Ok(outcome) =
                                cpu_tuning::apply_max_perf_pct(startup_max_perf, None).await
                        {
                            out.emit(FanCommandOutput::MaxPerfApplied {
                                outcome,
                                is_profile_load: true,
                            });
                            if let Ok(tel) =
                                tokio::task::spawn_blocking(read_cpu_power_telemetry).await
                            {
                                out.emit(FanCommandOutput::CpuTelemetryRead(tel));
                            }
                        }
                    }

                    let (ext_tx, mut ext_rx) = tokio::sync::mpsc::unbounded_channel();
                    let watcher = dbus::watch_external_fan_profile_changes(ext_tx);
                    let forwarder = async move {
                        while let Some((source, profile)) = ext_rx.recv().await {
                            out.emit(FanCommandOutput::ExternalProfileChanged { source, profile });
                        }
                    };
                    tokio::join!(watcher, forwarder);
                })
                .drop_on_shutdown()
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: FanMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            FanMsg::ChangeProfile(profile) => {
                if dbus::is_equivalent_fan_profile(profile, self.current_profile) {
                    return;
                }
                self.current_profile = profile;
                AppConfig::update(|c| c.active_profile_mut().fan_profile = profile as u32);
                self.emit_state(&sender);

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
                self.sync_radio_buttons(target);
                self.emit_state(&sender);

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
            FanMsg::SetFromTray(profile) => {
                if !self.asusd_available
                    || dbus::is_equivalent_fan_profile(profile, self.current_profile)
                {
                    return;
                }
                self.current_profile = profile;
                AppConfig::update(|c| c.active_profile_mut().fan_profile = profile as u32);
                self.sync_radio_buttons(profile);
                self.emit_state(&sender);

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
            FanMsg::LoadProfile {
                profile,
                cpu_max_perf_pct,
            } => {
                self.current_profile = profile;
                self.sync_radio_buttons(profile);
                self.emit_state(&sender);

                let target_max_perf = cpu_max_perf_pct.map(clamp_perf_pct);
                let prev_max_perf = self.cpu_max_perf_pct;
                if self.cpu_max_perf_pct != target_max_perf {
                    self.cpu_max_perf_pct = target_max_perf;
                    self.max_perf_presets = build_max_perf_presets(target_max_perf);
                    self.sync_max_perf_combo();
                }
                self.spawn_apply_max_perf(target_max_perf, prev_max_perf, 450, true, &sender);

                if !self.asusd_available {
                    return;
                }

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
            FanMsg::SelectMaxPerf(idx) => {
                let Some(&new_val) = self.max_perf_presets.get(idx as usize) else {
                    return;
                };
                if new_val == self.cpu_max_perf_pct {
                    return;
                }
                let prev_val = self.cpu_max_perf_pct;
                self.cpu_max_perf_pct = new_val;
                AppConfig::update(|c| c.active_profile_mut().cpu_max_perf_pct = new_val);
                self.spawn_apply_max_perf(new_val, prev_val, 0, false, &sender);
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
                            if let Ok(rpm) =
                                tokio::task::spawn_blocking(read_fan_rpm_telemetry).await
                            {
                                out.emit(FanCommandOutput::FanRpmRead(rpm));
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            FanMsg::SetPollingActive(active) => {
                let _ = self.fan_polling_tx.send_if_modified(|cur| {
                    if *cur == active {
                        false
                    } else {
                        *cur = active;
                        true
                    }
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
                self.emit_state(&sender);
            }
            FanCommandOutput::InitialProfile(profile) => {
                self.current_profile = profile;
                self.sync_radio_buttons(profile);
                self.emit_state(&sender);
                if self.cpu_max_perf_pct.is_none() {
                    self.spawn_refresh_cpu_telemetry(&sender);
                }
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
                self.emit_state(&sender);
                fan_osd::show(profile, AppConfig::load().fan_osd_enabled);

                // power-profiles-daemon updates EPP slightly asynchronously after asusd switches profile.
                self.spawn_refresh_cpu_telemetry(&sender);
            }
            FanCommandOutput::ExternalProfileChanged { source, profile } => {
                if dbus::is_equivalent_fan_profile(profile, self.current_profile) {
                    return;
                }
                self.current_profile = profile;
                AppConfig::update(|c| c.active_profile_mut().fan_profile = profile as u32);
                self.sync_radio_buttons(profile);
                self.emit_state(&sender);
                fan_osd::show(profile, AppConfig::load().fan_osd_enabled);

                match source {
                    dbus::ExternalProfileSource::Asusd => {
                        dbus::spawn_sync_power_profile(profile);
                    }
                    dbus::ExternalProfileSource::PowerProfiles => {
                        sender.command(move |out, shutdown| {
                            shutdown
                                .register(async move {
                                    if let Err(e) = dbus::set_platform_profile_only(profile).await {
                                        out.emit(FanCommandOutput::Error(e));
                                    }
                                })
                                .drop_on_shutdown()
                        });
                    }
                }
                self.spawn_refresh_cpu_telemetry(&sender);
            }
            FanCommandOutput::MaxPerfApplied {
                outcome,
                is_profile_load,
            } => match outcome {
                ApplyMaxPerfOutcome::Applied(pct) => {
                    if !is_profile_load {
                        tracing::info!("{}", t!("fan_cpu_max_perf_set", pct = pct.to_string()));
                    }
                }
                ApplyMaxPerfOutcome::PermissionDenied => {
                    self.pstate_writable = false;
                    if !is_profile_load {
                        let _ = sender.output(FanOutput::Error(
                            t!("fan_cpu_max_perf_permission_warning").to_string(),
                        ));
                    } else {
                        tracing::debug!(
                            "Skipping cpu_max_perf_pct apply on profile load: intel_pstate/max_perf_pct not writable"
                        );
                    }
                }
                ApplyMaxPerfOutcome::SkippedUnconfigured | ApplyMaxPerfOutcome::Unsupported => {}
            },
            FanCommandOutput::CpuTelemetryRead(tel) => {
                if let Some(summary) = format_cpu_telemetry(&tel) {
                    self.cpu_telemetry_subtitle = summary;
                    self.cpu_telemetry_available = true;
                } else {
                    self.cpu_telemetry_available = false;
                }
                self.max_perf_supported = tel.max_perf_pct.is_some();
                self.pstate_writable = tel.pstate_writable;
            }
            FanCommandOutput::FanRpmRead(tel) => {
                self.fan_rpm_subtitle = format_fan_rpm_status(&tel);
            }
            FanCommandOutput::Error(e) => {
                let _ = sender.output(FanOutput::Error(e));
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
            max_perf_pct: Some(80),
            min_perf_pct: Some(13),
            pstate_writable: true,
        };
        let formatted = format_cpu_telemetry(&populated).expect("should format telemetry");
        assert!(formatted.contains("balance_performance"));
        assert!(formatted.contains("powersave"));
        assert!(formatted.contains(&t!("fan_cpu_turbo_on").to_string()));
        assert!(formatted.contains(&t!("fan_cpu_max_perf", pct = "80").to_string()));
        // Normal low min_perf_pct (13%) should not clutter the summary
        assert!(!formatted.contains("13%"));

        // High min_perf_pct (>= 95%, e.g. tuned-ppd throughput-performance) should be surfaced
        let high_min = CpuPowerTelemetry {
            epp: Some("performance".to_string()),
            governor: Some("performance".to_string()),
            turbo_enabled: Some(true),
            max_perf_pct: Some(100),
            min_perf_pct: Some(100),
            pstate_writable: false,
        };
        let formatted_high = format_cpu_telemetry(&high_min).expect("should format high min");
        assert!(formatted_high.contains(&t!("fan_cpu_max_perf", pct = "100").to_string()));
        assert!(formatted_high.contains(&t!("fan_cpu_min_perf_high", pct = "100").to_string()));
    }

    #[test]
    fn test_format_fan_rpm_status() {
        use crate::sys_paths::FanSensorReading;

        let empty = FanRpmTelemetry::default();
        assert_eq!(
            format_fan_rpm_status(&empty),
            t!("fan_rpm_unavailable").to_string()
        );

        let zero = FanRpmTelemetry {
            fans: vec![FanSensorReading {
                label: "CPU".to_string(),
                rpm: 0,
            }],
        };
        assert_eq!(
            format_fan_rpm_status(&zero),
            format!("CPU: {}", t!("fan_rpm_off"))
        );

        let running = FanRpmTelemetry {
            fans: vec![FanSensorReading {
                label: "CPU".to_string(),
                rpm: 1766,
            }],
        };
        assert_eq!(format_fan_rpm_status(&running), "CPU: 1766 RPM");
    }
}
