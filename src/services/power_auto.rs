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

//! Automatic AC ↔ Battery profile switching driven by `org.freedesktop.UPower`.

use futures_util::StreamExt;
use relm4::Sender;
use tokio::sync::watch;

use crate::app::AppMsg;
use crate::services::config::AppConfig;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PowerAutoSettings {
    pub enabled: bool,
    pub ac_profile_id: Option<String>,
    pub battery_profile_id: Option<String>,
}

impl From<&AppConfig> for PowerAutoSettings {
    fn from(cfg: &AppConfig) -> Self {
        Self {
            enabled: cfg.power_auto_switch_enabled,
            ac_profile_id: cfg.ac_profile_id.clone(),
            battery_profile_id: cfg.battery_profile_id.clone(),
        }
    }
}

/// Resolves which profile ID should be activated for `on_battery`, if any.
///
/// Returns `None` when auto-switching is disabled, no profile is bound to the
/// current power source, the target profile no longer exists in `valid_ids`,
/// or the target profile is already active.
pub fn select_profile_for_power_state(
    settings: &PowerAutoSettings,
    on_battery: bool,
    current_active_id: &str,
    valid_ids: &[&str],
) -> Option<String> {
    if !settings.enabled {
        return None;
    }

    let target = if on_battery {
        settings.battery_profile_id.as_deref()
    } else {
        settings.ac_profile_id.as_deref()
    }?;

    if target.is_empty() || target == current_active_id || !valid_ids.contains(&target) {
        return None;
    }

    Some(target.to_string())
}

/// Returns `true` when a settings change should immediately re-evaluate and apply
/// the profile for the current `on_battery` state:
/// - `next.enabled` transitioned from `false` to `true`, OR
/// - auto-switch was already enabled and the binding for the *current* power source changed.
///
/// Editing `ac_profile_id` while currently on battery (or vice versa) returns `false`
/// so a temporary manual profile selection is not unexpectedly overridden.
pub fn should_apply_on_settings_change(
    prev: &PowerAutoSettings,
    next: &PowerAutoSettings,
    on_battery: bool,
) -> bool {
    if !next.enabled {
        return false;
    }
    if !prev.enabled {
        return true;
    }
    if on_battery {
        prev.battery_profile_id != next.battery_profile_id
    } else {
        prev.ac_profile_id != next.ac_profile_id
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower"
)]
trait UPower {
    #[zbus(property, name = "OnBattery")]
    fn on_battery(&self) -> zbus::Result<bool>;
}

fn maybe_emit_profile(
    app_sender: &Sender<AppMsg>,
    settings: &PowerAutoSettings,
    on_battery: bool,
) {
    if !settings.enabled {
        return;
    }
    let config = AppConfig::load();
    let valid_ids: Vec<&str> = config.profiles.iter().map(|p| p.id.as_str()).collect();
    if let Some(target_id) = select_profile_for_power_state(
        settings,
        on_battery,
        &config.active_profile_id,
        &valid_ids,
    ) {
        tracing::info!(
            "Power source changed (on_battery={}), switching to profile '{}'",
            on_battery,
            target_id
        );
        app_sender.emit(AppMsg::ActivateProfile(target_id));
    }
}

/// Watches `org.freedesktop.UPower.OnBattery` on the system D-Bus and switches
/// Ayuz profiles automatically whenever AC power is connected or unplugged.
pub async fn run(app_sender: Sender<AppMsg>, mut settings_rx: watch::Receiver<PowerAutoSettings>) {
    let conn = match crate::services::dbus::system_bus_connection().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("PowerAuto: system D-Bus unavailable: {}", e);
            return;
        }
    };

    let proxy = match UPowerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("PowerAuto: UPower proxy unavailable: {}", e);
            return;
        }
    };

    let mut prev_settings = settings_rx.borrow_and_update().clone();
    let mut last_on_battery = proxy.on_battery().await.ok();
    if let Some(on_bat) = last_on_battery {
        maybe_emit_profile(&app_sender, &prev_settings, on_bat);
    }

    let stream = proxy.receive_on_battery_changed().await;
    tokio::pin!(stream);

    loop {
        tokio::select! {
            res = settings_rx.changed() => {
                if res.is_err() {
                    break;
                }
                let settings = settings_rx.borrow_and_update().clone();
                if last_on_battery.is_none() {
                    last_on_battery = proxy.on_battery().await.ok();
                }
                if let Some(on_bat) = last_on_battery
                    && should_apply_on_settings_change(&prev_settings, &settings, on_bat)
                {
                    maybe_emit_profile(&app_sender, &settings, on_bat);
                }
                prev_settings = settings;
            }
            maybe = stream.next() => {
                let Some(changed) = maybe else {
                    break;
                };
                if let Ok(on_bat) = changed.get().await {
                    if last_on_battery == Some(on_bat) {
                        continue;
                    }
                    last_on_battery = Some(on_bat);
                    let settings = settings_rx.borrow().clone();
                    maybe_emit_profile(&app_sender, &settings, on_bat);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_profile_disabled() {
        let settings = PowerAutoSettings {
            enabled: false,
            ac_profile_id: Some("ac".to_string()),
            battery_profile_id: Some("bat".to_string()),
        };
        assert_eq!(
            select_profile_for_power_state(&settings, false, "other", &["ac", "bat", "other"]),
            None
        );
        assert_eq!(
            select_profile_for_power_state(&settings, true, "other", &["ac", "bat", "other"]),
            None
        );
    }

    #[test]
    fn test_select_profile_switches_ac_and_battery() {
        let settings = PowerAutoSettings {
            enabled: true,
            ac_profile_id: Some("ac".to_string()),
            battery_profile_id: Some("bat".to_string()),
        };

        assert_eq!(
            select_profile_for_power_state(&settings, false, "bat", &["ac", "bat"]),
            Some("ac".to_string())
        );
        assert_eq!(
            select_profile_for_power_state(&settings, true, "ac", &["ac", "bat"]),
            Some("bat".to_string())
        );
        // Already active
        assert_eq!(
            select_profile_for_power_state(&settings, false, "ac", &["ac", "bat"]),
            None
        );
        // Target deleted
        assert_eq!(
            select_profile_for_power_state(&settings, true, "ac", &["ac"]),
            None
        );
    }

    #[test]
    fn test_select_profile_partial_binding() {
        let settings = PowerAutoSettings {
            enabled: true,
            ac_profile_id: None,
            battery_profile_id: Some("eco".to_string()),
        };

        assert_eq!(
            select_profile_for_power_state(&settings, false, "default", &["default", "eco"]),
            None
        );
        assert_eq!(
            select_profile_for_power_state(&settings, true, "default", &["default", "eco"]),
            Some("eco".to_string())
        );
    }

    #[test]
    fn test_should_apply_on_settings_change() {
        let disabled = PowerAutoSettings {
            enabled: false,
            ac_profile_id: Some("ac".to_string()),
            battery_profile_id: Some("bat".to_string()),
        };
        let enabled = PowerAutoSettings {
            enabled: true,
            ..disabled.clone()
        };
        let edited_ac = PowerAutoSettings {
            ac_profile_id: Some("ac2".to_string()),
            ..enabled.clone()
        };

        // Turning off never applies
        assert!(!should_apply_on_settings_change(&enabled, &disabled, true));
        // Turning on applies for the current power source
        assert!(should_apply_on_settings_change(&disabled, &enabled, false));
        assert!(should_apply_on_settings_change(&disabled, &enabled, true));
        // Editing AC profile while on AC applies; while on battery does not
        assert!(should_apply_on_settings_change(&enabled, &edited_ac, false));
        assert!(!should_apply_on_settings_change(&enabled, &edited_ac, true));
        // Unchanged settings do not re-apply
        assert!(!should_apply_on_settings_change(&enabled, &enabled, true));
    }
}
