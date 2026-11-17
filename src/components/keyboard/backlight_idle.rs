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

use futures_util::StreamExt;
use gtk4 as gtk;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;
use tokio::sync::watch;

use super::kbd_dbus::{
    AmbientRules, KbdBacklightProxy, get_kbd_max_brightness, is_on_battery, read_ambient_lux,
    set_kbd_idle_dimmed,
};
use crate::services::commands::is_gnome_desktop;
use crate::services::config::AppConfig;

#[zbus::proxy(
    interface = "org.gnome.Mutter.IdleMonitor",
    default_service = "org.gnome.Mutter.IdleMonitor",
    default_path = "/org/gnome/Mutter/IdleMonitor/Core"
)]
trait MutterIdleMonitor {
    #[zbus(name = "AddIdleWatch")]
    fn add_idle_watch(&self, interval: u64) -> zbus::Result<u32>;

    #[zbus(name = "AddUserActiveWatch")]
    fn add_user_active_watch(&self) -> zbus::Result<u32>;

    #[zbus(name = "RemoveWatch")]
    fn remove_watch(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal, name = "WatchFired")]
    fn watch_fired(&self, id: u32) -> zbus::Result<()>;
}

/// Controls when the keyboard backlight idle-timeout is active.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) enum TimeoutMode {
    /// Backlight never times out automatically.
    #[default]
    Never,
    /// Timeout applies both on battery and when plugged in to AC power.
    BatteryAndAc,
    /// Timeout applies only when running on battery power.
    BatteryOnly,
}

impl From<u32> for TimeoutMode {
    fn from(v: u32) -> Self {
        match v {
            1 => Self::BatteryAndAc,
            2 => Self::BatteryOnly,
            _ => Self::Never,
        }
    }
}

const TIMEOUT_SECONDS: [u32; 3] = [60, 120, 300];

/// Wraps a shell command so it only runs while on battery power.
///
/// When `battery_only` is `true`, `base` is guarded by a conditional that checks whether any
/// power supply reports `online = 1`, skipping execution when the device is plugged in to AC
/// or USB-C PD power. The guard tolerates a multi-statement `base` (statements separated by `;`).
fn battery_guard(base: &str, battery_only: bool) -> String {
    if battery_only {
        format!(
            "if ! grep -qx 1 /sys/class/power_supply/*/online 2>/dev/null; \
             then {base}; fi"
        )
    } else {
        base.to_string()
    }
}

/// Builds a `busctl` command string to set keyboard backlight brightness via UPower.
///
/// When `battery_only` is `true`, the command is wrapped in a shell conditional that reads
/// the first `online` sysfs file to skip execution when the device is plugged in to AC power.
fn busctl_brightness_cmd(value: i32, battery_only: bool) -> String {
    let base = format!(
        "busctl call --system org.freedesktop.UPower \
         /org/freedesktop/UPower/KbdBacklight \
         org.freedesktop.UPower.KbdBacklight SetBrightness i {value}"
    );
    battery_guard(&base, battery_only)
}

/// Builds the `busctl` resume command `swayidle` runs when the user returns from idle.
fn busctl_resume_cmd(battery_only: bool, rules: AmbientRules, max_brightness: i32) -> String {
    let max_b = max_brightness.max(1);
    if !rules.auto_brighten && !rules.auto_dim {
        return busctl_brightness_cmd(max_b, battery_only);
    }

    // Read the live ambient level held by the ambient loop's sensor claim. `$lvl` is empty when
    // the sensor is unavailable, in which case the rules below leave `v` at `max_b`.
    let read_lux = "lvl=$(busctl --system get-property net.hadess.SensorProxy \
        /net/hadess/SensorProxy net.hadess.SensorProxy LightLevel 2>/dev/null | awk '{print $2}')";

    // Default to full brightness
    let mut decide = format!("v={max_b}");
    if rules.auto_dim {
        decide.push_str(&format!(
            "; if [ -n \"$lvl\" ] && awk \"BEGIN{{exit !($lvl > {})}}\"; then v=0; fi",
            rules.dim_threshold
        ));
    }
    if rules.auto_brighten {
        decide.push_str(&format!(
            "; if [ -n \"$lvl\" ] && awk \"BEGIN{{exit !($lvl >= {})}}\"; then v=0; fi",
            rules.brighten_threshold
        ));
    }

    let set = "busctl call --system org.freedesktop.UPower \
        /org/freedesktop/UPower/KbdBacklight \
        org.freedesktop.UPower.KbdBacklight SetBrightness i $v";

    battery_guard(&format!("{read_lux}; {decide}; {set}"), battery_only)
}

/// Computes the target keyboard backlight brightness when returning from idle on GNOME.
pub(super) fn decide_resume_brightness(
    saved_brightness: i32,
    max_brightness: i32,
    rules: AmbientRules,
    ambient_lux: Option<f64>,
) -> i32 {
    let max_b = max_brightness.max(1);
    let restored = if saved_brightness > 0 {
        saved_brightness.min(max_b)
    } else {
        max_b
    };

    if !rules.auto_brighten && !rules.auto_dim {
        return restored;
    }

    let Some(lvl) = ambient_lux else {
        return restored;
    };

    if rules.auto_dim && lvl > rules.dim_threshold {
        return 0;
    }
    if rules.auto_brighten {
        if lvl >= rules.brighten_threshold {
            return 0;
        }
        return max_b;
    }

    restored
}

pub struct BacklightIdleModel {
    timeout_mode: TimeoutMode,
    battery_ac_index: u32,
    battery_only_index: u32,
    check_never: gtk::CheckButton,
    check_battery_and_ac: gtk::CheckButton,
    check_battery_only: gtk::CheckButton,
    dropdown_battery_and_ac: gtk::DropDown,
    dropdown_battery_only: gtk::DropDown,
    idle_stop_tx: Option<watch::Sender<bool>>,
    rules_tx: Option<watch::Sender<AmbientRules>>,
}

#[derive(Debug)]
pub enum BacklightIdleMsg {
    ChangeMode(TimeoutMode),
    BatteryAndAcTimeChanged(u32),
    BatteryOnlyTimeChanged(u32),
    LoadProfile {
        mode: u32,
        ac_index: u32,
        battery_index: u32,
    },
    /// Rebuild the running `swayidle` so its resume command picks up changed ambient
    /// automation settings (issue #39). No-op when no timeout is active.
    AmbientChanged,
}

#[derive(Debug)]
pub enum BacklightIdleCommandOutput {
    Error(String),
}

#[relm4::component(pub)]
impl Component for BacklightIdleModel {
    type Init = ();
    type Input = BacklightIdleMsg;
    type Output = String;
    type CommandOutput = BacklightIdleCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &t!("sleep_group_title"),
            set_description: Some(&t!("sleep_group_desc")),

            add = &adw::ActionRow {
                set_title: &t!("sleep_mode_never_title"),
                set_subtitle: &t!("sleep_mode_never_subtitle"),
                add_prefix = &model.check_never.clone(),
                set_activatable_widget: Some(&model.check_never),
            },

            add = &adw::ActionRow {
                set_title: &t!("sleep_mode_always_title"),
                add_prefix = &model.check_battery_and_ac.clone(),
                set_activatable_widget: Some(&model.check_battery_and_ac),
                add_suffix = &model.dropdown_battery_and_ac.clone() -> gtk::DropDown {
                    set_valign: gtk::Align::Center,
                    #[watch]
                    set_sensitive: model.timeout_mode == TimeoutMode::BatteryAndAc,
                },
            },

            add = &adw::ActionRow {
                set_title: &t!("sleep_mode_battery_title"),
                add_prefix = &model.check_battery_only.clone(),
                set_activatable_widget: Some(&model.check_battery_only),
                add_suffix = &model.dropdown_battery_only.clone() -> gtk::DropDown {
                    set_valign: gtk::Align::Center,
                    #[watch]
                    set_sensitive: model.timeout_mode == TimeoutMode::BatteryOnly,
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = AppConfig::load();
        let p = config.active_profile();
        let mode = TimeoutMode::from(p.kbd_timeout_mode);

        let check_never = gtk::CheckButton::new();
        let check_battery_and_ac = gtk::CheckButton::new();
        let check_battery_only = gtk::CheckButton::new();
        check_battery_and_ac.set_group(Some(&check_never));
        check_battery_only.set_group(Some(&check_never));

        match mode {
            TimeoutMode::Never => check_never.set_active(true),
            TimeoutMode::BatteryAndAc => check_battery_and_ac.set_active(true),
            TimeoutMode::BatteryOnly => check_battery_only.set_active(true),
        }

        let t1 = t!("sleep_timeout_1min");
        let t2 = t!("sleep_timeout_2min");
        let t5 = t!("sleep_timeout_5min");
        let time_options = gtk::StringList::new(&[&t1, &t2, &t5]);
        let dropdown_battery_and_ac =
            gtk::DropDown::new(Some(time_options.clone()), gtk::Expression::NONE);
        let dropdown_battery_only = gtk::DropDown::new(Some(time_options), gtk::Expression::NONE);
        dropdown_battery_and_ac.set_selected(p.kbd_timeout_battery_ac_index);
        dropdown_battery_only.set_selected(p.kbd_timeout_battery_only_index);

        for (btn, mode_val) in [
            (&check_never, TimeoutMode::Never),
            (&check_battery_and_ac, TimeoutMode::BatteryAndAc),
            (&check_battery_only, TimeoutMode::BatteryOnly),
        ] {
            let sender = sender.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    sender.input(BacklightIdleMsg::ChangeMode(mode_val));
                }
            });
        }

        {
            let sender = sender.clone();
            dropdown_battery_and_ac.connect_selected_notify(move |dd| {
                sender.input(BacklightIdleMsg::BatteryAndAcTimeChanged(dd.selected()));
            });
        }
        {
            let sender = sender.clone();
            dropdown_battery_only.connect_selected_notify(move |dd| {
                sender.input(BacklightIdleMsg::BatteryOnlyTimeChanged(dd.selected()));
            });
        }

        let mut model = BacklightIdleModel {
            timeout_mode: mode,
            battery_ac_index: p.kbd_timeout_battery_ac_index,
            battery_only_index: p.kbd_timeout_battery_only_index,
            check_never,
            check_battery_and_ac,
            check_battery_only,
            dropdown_battery_and_ac,
            dropdown_battery_only,
            idle_stop_tx: None,
            rules_tx: None,
        };

        let widgets = view_output!();
        model.apply_timeout(mode, &sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: BacklightIdleMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            BacklightIdleMsg::ChangeMode(mode) => {
                if mode == self.timeout_mode {
                    return;
                }
                self.timeout_mode = mode;
                AppConfig::update(|c| c.active_profile_mut().kbd_timeout_mode = mode as u32);
                self.apply_timeout(mode, &sender);
            }
            BacklightIdleMsg::BatteryAndAcTimeChanged(index) => {
                if index == self.battery_ac_index {
                    return;
                }
                self.battery_ac_index = index;
                AppConfig::update(|c| c.active_profile_mut().kbd_timeout_battery_ac_index = index);
                if self.timeout_mode == TimeoutMode::BatteryAndAc {
                    self.apply_timeout(TimeoutMode::BatteryAndAc, &sender);
                }
            }
            BacklightIdleMsg::BatteryOnlyTimeChanged(index) => {
                if index == self.battery_only_index {
                    return;
                }
                self.battery_only_index = index;
                AppConfig::update(|c| {
                    c.active_profile_mut().kbd_timeout_battery_only_index = index
                });
                if self.timeout_mode == TimeoutMode::BatteryOnly {
                    self.apply_timeout(TimeoutMode::BatteryOnly, &sender);
                }
            }
            BacklightIdleMsg::LoadProfile {
                mode,
                ac_index,
                battery_index,
            } => {
                let timeout_mode = TimeoutMode::from(mode);
                self.timeout_mode = timeout_mode;
                self.battery_ac_index = ac_index;
                self.battery_only_index = battery_index;
                self.check_never
                    .set_active(timeout_mode == TimeoutMode::Never);
                self.check_battery_and_ac
                    .set_active(timeout_mode == TimeoutMode::BatteryAndAc);
                self.check_battery_only
                    .set_active(timeout_mode == TimeoutMode::BatteryOnly);
                self.dropdown_battery_and_ac.set_selected(ac_index);
                self.dropdown_battery_only.set_selected(battery_index);
                self.apply_timeout(timeout_mode, &sender);
            }
            BacklightIdleMsg::AmbientChanged => {
                if self.timeout_mode == TimeoutMode::Never {
                    return;
                }
                if is_gnome_desktop()
                    && let Some(tx) = &self.rules_tx
                {
                    let config = AppConfig::load();
                    let p = config.active_profile();
                    let rules = AmbientRules {
                        auto_brighten: p.kbd_brighten_active,
                        brighten_threshold: p.kbd_brighten_threshold,
                        auto_dim: p.kbd_dim_active,
                        dim_threshold: p.kbd_dim_threshold,
                    };
                    if tx.send(rules).is_ok() {
                        return;
                    }
                }
                let mode = self.timeout_mode;
                self.apply_timeout(mode, &sender);
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: BacklightIdleCommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            BacklightIdleCommandOutput::Error(e) => {
                let _ = sender.output(e);
            }
        }
    }
}

impl BacklightIdleModel {
    /// (Re)starts the idle-timeout monitor with the current timeout settings.
    ///
    /// Stops any previously running idle-monitor task before starting a new one.
    /// Does nothing (and stops the old task) when `mode` is [`TimeoutMode::Never`].
    ///
    /// Uses `org.gnome.Mutter.IdleMonitor` + `org.freedesktop.UPower.KbdBacklight` via `zbus`
    /// on GNOME Wayland (where `swayidle` fails because Mutter lacks `ext-idle-notify-v1`),
    /// and retains `swayidle` on KDE/COSMIC/wlroots compositors.
    fn apply_timeout(&mut self, mode: TimeoutMode, sender: &ComponentSender<BacklightIdleModel>) {
        if let Some(tx) = self.idle_stop_tx.take() {
            let _ = tx.send(false);
        }
        self.rules_tx = None;
        set_kbd_idle_dimmed(false);

        if mode == TimeoutMode::Never {
            return;
        }

        let seconds = match mode {
            TimeoutMode::Never => {
                unreachable!("TimeoutMode::Never is handled by the early return above")
            }
            TimeoutMode::BatteryAndAc => {
                let idx = self.dropdown_battery_and_ac.selected() as usize;
                *TIMEOUT_SECONDS.get(idx).unwrap_or(&60)
            }
            TimeoutMode::BatteryOnly => {
                let idx = self.dropdown_battery_only.selected() as usize;
                *TIMEOUT_SECONDS.get(idx).unwrap_or(&60)
            }
        };

        let battery_only = mode == TimeoutMode::BatteryOnly;

        // Make the resume restore ambient-aware so returning from idle in a lit room does not
        // force the backlight to full brightness (issue #39).
        let config = AppConfig::load();
        let p = config.active_profile();
        let rules = AmbientRules {
            auto_brighten: p.kbd_brighten_active,
            brighten_threshold: p.kbd_brighten_threshold,
            auto_dim: p.kbd_dim_active,
            dim_threshold: p.kbd_dim_threshold,
        };

        let (stop_tx, stop_rx) = watch::channel(true);
        let (rules_tx, rules_rx) = watch::channel(rules);
        let cmd_sender = sender.command_sender().clone();

        if is_gnome_desktop() {
            tokio::spawn(run_gnome_idle_monitor(
                seconds,
                battery_only,
                rules_rx,
                stop_rx,
                cmd_sender,
            ));
            self.rules_tx = Some(rules_tx);
        } else {
            tokio::spawn(run_swayidle(
                seconds,
                battery_only,
                rules,
                stop_rx,
                cmd_sender,
            ));
        }

        self.idle_stop_tx = Some(stop_tx);
    }
}

async fn run_gnome_idle_monitor(
    seconds: u32,
    battery_only: bool,
    mut rules_rx: watch::Receiver<AmbientRules>,
    mut stop_rx: watch::Receiver<bool>,
    cmd_sender: relm4::Sender<BacklightIdleCommandOutput>,
) {
    let mut rules = *rules_rx.borrow_and_update();
    let session_conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            cmd_sender.emit(BacklightIdleCommandOutput::Error(
                t!("error_idle_monitor", error = e.to_string()).to_string(),
            ));
            return;
        }
    };

    let idle_proxy = match MutterIdleMonitorProxy::new(&session_conn).await {
        Ok(p) => p,
        Err(e) => {
            cmd_sender.emit(BacklightIdleCommandOutput::Error(
                t!("error_idle_monitor", error = e.to_string()).to_string(),
            ));
            return;
        }
    };

    let watch_fired = match idle_proxy.receive_watch_fired().await {
        Ok(s) => s,
        Err(e) => {
            cmd_sender.emit(BacklightIdleCommandOutput::Error(
                t!("error_idle_monitor", error = e.to_string()).to_string(),
            ));
            return;
        }
    };

    let interval_ms = u64::from(seconds) * 1000;
    let idle_watch_id = match idle_proxy.add_idle_watch(interval_ms).await {
        Ok(id) => id,
        Err(e) => {
            cmd_sender.emit(BacklightIdleCommandOutput::Error(
                t!("error_idle_monitor", error = e.to_string()).to_string(),
            ));
            return;
        }
    };

    if !*stop_rx.borrow_and_update() {
        let _ = idle_proxy.remove_watch(idle_watch_id).await;
        return;
    }

    let sys_conn = match zbus::Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            let _ = idle_proxy.remove_watch(idle_watch_id).await;
            cmd_sender.emit(BacklightIdleCommandOutput::Error(
                t!("error_idle_monitor", error = e.to_string()).to_string(),
            ));
            return;
        }
    };

    let kbd_proxy = match KbdBacklightProxy::new(&sys_conn).await {
        Ok(p) => p,
        Err(e) => {
            let _ = idle_proxy.remove_watch(idle_watch_id).await;
            cmd_sender.emit(BacklightIdleCommandOutput::Error(
                t!("error_idle_monitor", error = e.to_string()).to_string(),
            ));
            return;
        }
    };

    let max_brightness = kbd_proxy
        .get_max_brightness()
        .await
        .ok()
        .filter(|&v| v > 0)
        .unwrap_or(3);

    let mut active_watch_id: Option<u32> = None;
    let mut dimmed_from: Option<i32> = None;

    tokio::pin!(watch_fired);

    loop {
        tokio::select! {
            res = stop_rx.changed() => {
                if res.is_err() || !*stop_rx.borrow() {
                    break;
                }
            }
            Ok(()) = rules_rx.changed() => {
                rules = *rules_rx.borrow_and_update();
            }
            maybe_sig = watch_fired.next() => {
                let Some(sig) = maybe_sig else {
                    break;
                };
                let Ok(args) = sig.args() else {
                    continue;
                };
                let fired_id = args.id;

                if fired_id == idle_watch_id {
                    if battery_only && !is_on_battery(&sys_conn).await {
                        continue;
                    }
                    if dimmed_from.is_some() {
                        continue;
                    }
                    let cur = kbd_proxy.get_brightness().await.unwrap_or(max_brightness);
                    if active_watch_id.is_none() {
                        match idle_proxy.add_user_active_watch().await {
                            Ok(id) => active_watch_id = Some(id),
                            Err(e) => {
                                tracing::warn!("Mutter AddUserActiveWatch failed: {e}");
                                continue;
                            }
                        }
                    }
                    if cur <= 0 {
                        // Already off (e.g. dimmed by ambient sensor in a bright room), but record
                        // the idle state so `auto_backlight` does not turn the keyboard on while
                        // the user remains idle if the room darkens.
                        set_kbd_idle_dimmed(true);
                        dimmed_from = Some(0);
                    } else if kbd_proxy.set_brightness(0).await.is_ok() {
                        set_kbd_idle_dimmed(true);
                        dimmed_from = Some(cur);
                    } else if let Some(aid) = active_watch_id.take() {
                        let _ = idle_proxy.remove_watch(aid).await;
                    }
                } else if active_watch_id == Some(fired_id) {
                    active_watch_id = None;
                    set_kbd_idle_dimmed(false);
                    if let Some(prev) = dimmed_from.take() {
                        let ambient_lux = if rules.auto_brighten || rules.auto_dim {
                            read_ambient_lux(&sys_conn).await
                        } else {
                            None
                        };
                        if prev == 0 && !rules.auto_brighten {
                            continue;
                        }
                        let target = decide_resume_brightness(
                            prev,
                            max_brightness,
                            rules,
                            ambient_lux,
                        );
                        if target > 0
                            && let Err(e) = kbd_proxy.set_brightness(target).await
                        {
                            tracing::warn!("Failed to restore keyboard backlight: {e}");
                        }
                    }
                }
            }
        }
    }

    set_kbd_idle_dimmed(false);
    let _ = idle_proxy.remove_watch(idle_watch_id).await;
    if let Some(aid) = active_watch_id {
        let _ = idle_proxy.remove_watch(aid).await;
    }
}

async fn run_swayidle(
    seconds: u32,
    battery_only: bool,
    rules: AmbientRules,
    mut stop_rx: watch::Receiver<bool>,
    cmd_sender: relm4::Sender<BacklightIdleCommandOutput>,
) {
    let max_brightness = get_kbd_max_brightness().await;
    if !*stop_rx.borrow_and_update() {
        return;
    }

    let timeout_cmd = busctl_brightness_cmd(0, battery_only);
    let resume_cmd = busctl_resume_cmd(battery_only, rules, max_brightness);
    let seconds_str = seconds.to_string();

    let mut child = match tokio::process::Command::new("swayidle")
        .kill_on_drop(true)
        .args([
            "-w",
            "timeout",
            &seconds_str,
            &timeout_cmd,
            "resume",
            &resume_cmd,
        ])
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            cmd_sender.emit(BacklightIdleCommandOutput::Error(
                t!("error_swayidle_start", error = e.to_string()).to_string(),
            ));
            return;
        }
    };

    tokio::select! {
        res = child.wait() => {
            if let Err(e) = res {
                cmd_sender.emit(BacklightIdleCommandOutput::Error(
                    t!("error_swayidle_wait", error = e.to_string()).to_string(),
                ));
            }
        }
        _ = stop_rx.changed() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_rules() -> AmbientRules {
        AmbientRules {
            auto_brighten: false,
            brighten_threshold: 12.0,
            auto_dim: false,
            dim_threshold: 35.0,
        }
    }

    #[test]
    fn test_decide_resume_brightness_restores_saved_level() {
        let rules = disabled_rules();
        assert_eq!(decide_resume_brightness(2, 3, rules, None), 2);
        assert_eq!(decide_resume_brightness(3, 3, rules, None), 3);
        assert_eq!(decide_resume_brightness(0, 3, rules, None), 3);
        assert_eq!(decide_resume_brightness(5, 3, rules, None), 3);
        assert_eq!(decide_resume_brightness(128, 255, rules, None), 128);
    }

    #[test]
    fn test_decide_resume_brightness_ambient_aware() {
        let rules = AmbientRules {
            auto_brighten: true,
            brighten_threshold: 12.0,
            auto_dim: true,
            dim_threshold: 35.0,
        };
        // Dark room -> restore to max
        assert_eq!(decide_resume_brightness(2, 3, rules, Some(5.0)), 3);
        assert_eq!(decide_resume_brightness(2, 255, rules, Some(5.0)), 255);
        // Bright room -> stay off
        assert_eq!(decide_resume_brightness(2, 3, rules, Some(50.0)), 0);
        // Above brighten threshold -> stay off when auto_brighten is active
        assert_eq!(decide_resume_brightness(2, 3, rules, Some(15.0)), 0);
        // Sensor unavailable -> restore saved level (or max if unknown/zero)
        assert_eq!(decide_resume_brightness(2, 3, rules, None), 2);
        assert_eq!(decide_resume_brightness(0, 3, rules, None), 3);

        // Auto-dim only -> keep saved level when room is not bright
        let dim_only = AmbientRules {
            auto_brighten: false,
            ..rules
        };
        assert_eq!(decide_resume_brightness(1, 3, dim_only, Some(10.0)), 1);
        assert_eq!(decide_resume_brightness(1, 3, dim_only, Some(50.0)), 0);
        assert_eq!(decide_resume_brightness(1, 3, dim_only, None), 1);
    }

    #[test]
    fn test_busctl_resume_cmd_uses_dynamic_max() {
        let cmd = busctl_resume_cmd(false, disabled_rules(), 255);
        assert!(cmd.contains("SetBrightness i 255"));

        let rules = AmbientRules {
            auto_brighten: true,
            brighten_threshold: 12.0,
            auto_dim: false,
            dim_threshold: 35.0,
        };
        let ambient_cmd = busctl_resume_cmd(true, rules, 2);
        assert!(ambient_cmd.contains("v=2"));
        assert!(ambient_cmd.contains("! grep -qx 1 /sys/class/power_supply/*/online"));
    }

    #[test]
    fn test_kbd_idle_dimmed_shared_flag() {
        use super::super::kbd_dbus::is_kbd_idle_dimmed;
        set_kbd_idle_dimmed(true);
        assert!(is_kbd_idle_dimmed());
        set_kbd_idle_dimmed(false);
        assert!(!is_kbd_idle_dimmed());
    }
}
