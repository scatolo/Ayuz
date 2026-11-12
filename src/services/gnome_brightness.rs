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

use std::collections::HashMap;
use std::ops::Deref;
use zbus::zvariant::{OwnedValue, Value};

/// Legacy GNOME Settings Daemon Power Screen interface (GNOME < 47 / X11).
#[zbus::proxy(
    interface = "org.gnome.SettingsDaemon.Power.Screen",
    default_service = "org.gnome.SettingsDaemon.Power",
    default_path = "/org/gnome/SettingsDaemon/Power"
)]
pub trait GnomeScreenBrightness {
    #[zbus(name = "StepUp")]
    fn step_up(&self) -> zbus::Result<(u32, String)>;

    #[zbus(name = "StepDown")]
    fn step_down(&self) -> zbus::Result<(u32, String)>;

    #[zbus(property, name = "Brightness")]
    fn brightness(&self) -> zbus::Result<i32>;

    #[zbus(property, name = "Brightness")]
    fn set_brightness(&self, value: i32) -> zbus::Result<()>;
}

/// GNOME Mutter DisplayConfig interface (GNOME 47+ Wayland).
#[zbus::proxy(
    interface = "org.gnome.Mutter.DisplayConfig",
    default_service = "org.gnome.Mutter.DisplayConfig",
    default_path = "/org/gnome/Mutter/DisplayConfig"
)]
pub trait MutterDisplayConfig {
    #[zbus(property, name = "Backlight")]
    fn backlight(&self) -> zbus::Result<(u32, Vec<HashMap<String, OwnedValue>>)>;

    #[zbus(name = "SetBacklight")]
    fn set_backlight(&self, serial: u32, connector: &str, value: i32) -> zbus::Result<()>;
}

/// Helper function to unwrap nested `Value::Value` variants.
fn unwrap_val<'a>(mut val: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(inner) = val {
        val = inner;
    }
    val
}

/// Extracts an `i32` from a variant map, tolerating various integer types.
pub(crate) fn extract_i32(map: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    map.get(key).and_then(|ov| match unwrap_val(ov.deref()) {
        Value::I32(v) => Some(*v),
        Value::U32(v) => Some(*v as i32),
        Value::I16(v) => Some(*v as i32),
        Value::U16(v) => Some(*v as i32),
        Value::I64(v) => Some(*v as i32),
        Value::U64(v) => Some(*v as i32),
        _ => None,
    })
}

/// Extracts a `bool` from a variant map.
pub(crate) fn extract_bool(map: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    map.get(key).and_then(|ov| match unwrap_val(ov.deref()) {
        Value::Bool(v) => Some(*v),
        _ => None,
    })
}

/// Extracts a `String` from a variant map.
pub(crate) fn extract_string(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    map.get(key).and_then(|ov| match unwrap_val(ov.deref()) {
        Value::Str(s) => Some(s.as_str().to_string()),
        _ => None,
    })
}

/// Calculates the new backlight value clamped between `min` and `max`.
/// Guarantees at least a 1-unit step when `delta_percent != 0`.
pub fn calculate_next_backlight(min: i32, max: i32, cur: i32, delta_percent: i32) -> i32 {
    let span = (max - min).max(1);
    let mut step = ((span as f64) * (delta_percent as f64) / 100.0).round() as i32;
    if step == 0 && delta_percent != 0 {
        step = if delta_percent > 0 { 1 } else { -1 };
    }
    (cur + step).clamp(min, max)
}

/// Adjusts the screen brightness by `delta_percent` (e.g. +5 or -5) via GNOME D-Bus interfaces.
///
/// Tries `org.gnome.SettingsDaemon.Power.Screen` first; if unavailable (such as on GNOME 47+ Wayland),
/// falls back to `org.gnome.Mutter.DisplayConfig`.
/// Callers should fall back to `brightnessctl` if an error is returned.
pub async fn adjust_brightness_relative(delta_percent: i32) -> Result<(), String> {
    let conn = zbus::Connection::session().await.map_err(|e| e.to_string())?;

    // 1. Try legacy SettingsDaemon.Power.Screen
    if let Ok(gsd) = GnomeScreenBrightnessProxy::new(&conn).await {
        let step_res = if delta_percent > 0 {
            gsd.step_up().await.map(|_| ())
        } else if delta_percent < 0 {
            gsd.step_down().await.map(|_| ())
        } else {
            Ok(())
        };

        if step_res.is_ok() {
            return Ok(());
        }

        if let Ok(cur) = gsd.brightness().await {
            let next = (cur + delta_percent).clamp(0, 100);
            if gsd.set_brightness(next).await.is_ok() {
                return Ok(());
            }
        }
    }

    // 2. Fall back to Mutter DisplayConfig
    let mutter = MutterDisplayConfigProxy::new(&conn)
        .await
        .map_err(|e| format!("Mutter DisplayConfig proxy unavailable: {e}"))?;

    adjust_mutter_backlight(&mutter, delta_percent).await
}

async fn adjust_mutter_backlight(
    mutter: &MutterDisplayConfigProxy<'_>,
    delta_percent: i32,
) -> Result<(), String> {
    let (serial, backlights) = mutter
        .backlight()
        .await
        .map_err(|e| format!("Failed to read Mutter Backlight property: {e}"))?;

    let active_entry = backlights
        .iter()
        .find(|map| extract_bool(map, "active").unwrap_or(false))
        .or_else(|| backlights.first())
        .ok_or_else(|| "No backlight device found in Mutter DisplayConfig".to_string())?;

    let connector = extract_string(active_entry, "connector")
        .ok_or_else(|| "Missing 'connector' in Mutter backlight device".to_string())?;
    let min = extract_i32(active_entry, "min").unwrap_or(0);
    let max = extract_i32(active_entry, "max").unwrap_or(100);
    let cur = extract_i32(active_entry, "value").unwrap_or(min);

    let next = calculate_next_backlight(min, max, cur, delta_percent);

    mutter
        .set_backlight(serial, &connector, next)
        .await
        .map_err(|e| format!("Mutter SetBacklight failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_next_backlight_standard() {
        // ASUS Zenbook UX3405CA real values: min 4, max 496, cur 496
        assert_eq!(calculate_next_backlight(4, 496, 496, -5), 471);
        assert_eq!(calculate_next_backlight(4, 496, 471, 5), 496);
    }

    #[test]
    fn test_calculate_next_backlight_clamping() {
        assert_eq!(calculate_next_backlight(4, 496, 496, 5), 496);
        assert_eq!(calculate_next_backlight(4, 496, 10, -10), 4);
    }

    #[test]
    fn test_calculate_next_backlight_small_span() {
        // Small span 0..10 where 5% is 0.5 (rounds to 1 or minimum 1)
        assert_eq!(calculate_next_backlight(0, 10, 5, 5), 6);
        assert_eq!(calculate_next_backlight(0, 10, 5, -5), 4);
    }

    #[test]
    fn test_extract_helpers() {
        let mut map = HashMap::new();
        map.insert("connector".to_string(), OwnedValue::from("eDP-1"));
        map.insert("active".to_string(), OwnedValue::from(true));
        map.insert("min".to_string(), OwnedValue::from(4i32));
        map.insert("max".to_string(), OwnedValue::from(496i32));
        map.insert("value".to_string(), OwnedValue::from(450i32));

        assert_eq!(extract_string(&map, "connector").as_deref(), Some("eDP-1"));
        assert_eq!(extract_bool(&map, "active"), Some(true));
        assert_eq!(extract_i32(&map, "min"), Some(4));
        assert_eq!(extract_i32(&map, "max"), Some(496));
        assert_eq!(extract_i32(&map, "value"), Some(450));
        assert_eq!(extract_i32(&map, "nonexistent"), None);
    }
}
