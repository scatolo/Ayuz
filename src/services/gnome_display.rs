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

use crate::services::commands::{is_gnome_desktop, is_kde_desktop, run_command_blocking};

/// `ApplyMonitorsConfig` method: verify configuration only (`0`), apply temporarily (`1`),
/// or persist to `~/.config/monitors.xml` (`2`).
const MUTTER_CONFIG_METHOD_PERSISTENT: u32 = 2;

#[allow(clippy::type_complexity)]
type MonitorSpec = (String, String, String, String);
#[allow(clippy::type_complexity)]
type RawDisplayMode = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);
#[allow(clippy::type_complexity)]
type RawMonitor = (MonitorSpec, Vec<RawDisplayMode>, HashMap<String, OwnedValue>);
#[allow(clippy::type_complexity)]
type RawLogicalMonitor = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<MonitorSpec>,
    HashMap<String, OwnedValue>,
);
#[allow(clippy::type_complexity)]
type CurrentDisplayState = (
    u32,
    Vec<RawMonitor>,
    Vec<RawLogicalMonitor>,
    HashMap<String, OwnedValue>,
);
#[allow(clippy::type_complexity)]
type ApplyMonitorEntry = (String, String, HashMap<String, OwnedValue>);
#[allow(clippy::type_complexity)]
type ApplyLogicalMonitorEntry = (i32, i32, f64, u32, bool, Vec<ApplyMonitorEntry>);

#[allow(clippy::type_complexity)]
#[zbus::proxy(
    interface = "org.gnome.Mutter.DisplayConfig",
    default_service = "org.gnome.Mutter.DisplayConfig",
    default_path = "/org/gnome/Mutter/DisplayConfig"
)]
trait MutterDisplayConfig {
    #[zbus(name = "GetCurrentState")]
    fn get_current_state(&self) -> zbus::Result<CurrentDisplayState>;

    #[zbus(name = "ApplyMonitorsConfig")]
    fn apply_monitors_config(
        &self,
        serial: u32,
        method: u32,
        logical_monitors: Vec<ApplyLogicalMonitorEntry>,
        properties: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
}

/// Normalized representation of a display mode returned by Mutter or `kscreen-doctor`.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayModeInfo {
    pub id: String,
    pub width: i32,
    pub height: i32,
    pub refresh_rate: f64,
    pub is_current: bool,
    pub is_preferred: bool,
    pub is_interlaced: bool,
}

/// Summary of the built-in laptop panel and its available refresh rates at current resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalDisplayInfo {
    pub connector: String,
    pub current_width: i32,
    pub current_height: i32,
    pub current_hz: u32,
    pub available_hz: Vec<u32>,
}

/// Parsed `kscreen-doctor -j` representation for an internal display output.
#[derive(Debug, Clone, PartialEq)]
pub struct KscreenOutputInfo {
    pub info: InternalDisplayInfo,
    pub modes: Vec<DisplayModeInfo>,
}

/// Rounds a floating-point refresh rate (e.g. `59.999`, `119.880`) to integer Hz.
pub fn round_hz(rate: f64) -> u32 {
    if !rate.is_finite() || rate <= 0.0 {
        0
    } else {
        rate.round() as u32
    }
}

/// Returns `true` when `connector` (or Mutter's `is-builtin` flag) identifies an internal panel.
pub fn is_internal_connector(connector: &str, is_builtin: Option<bool>) -> bool {
    is_builtin == Some(true)
        || connector.starts_with("eDP")
        || connector.starts_with("LVDS")
        || connector.starts_with("DSI")
}

/// Summarizes the current resolution, active refresh rate, and all supported refresh rates
/// for the current resolution from a monitor's mode list.
pub fn summarize_internal_display(
    connector: &str,
    modes: &[DisplayModeInfo],
) -> Option<InternalDisplayInfo> {
    let current = modes
        .iter()
        .find(|m| m.is_current)
        .or_else(|| modes.iter().find(|m| m.is_preferred))
        .or_else(|| modes.first())?;

    let w = current.width;
    let h = current.height;
    let current_hz = round_hz(current.refresh_rate);
    if w <= 0 || h <= 0 || current_hz == 0 {
        return None;
    }

    let has_progressive = modes
        .iter()
        .any(|m| m.width == w && m.height == h && !m.is_interlaced);

    let mut available_hz: Vec<u32> = modes
        .iter()
        .filter(|m| m.width == w && m.height == h && (!has_progressive || !m.is_interlaced))
        .map(|m| round_hz(m.refresh_rate))
        .filter(|&hz| hz > 0)
        .collect();
    available_hz.sort_unstable();
    available_hz.dedup();

    Some(InternalDisplayInfo {
        connector: connector.to_string(),
        current_width: w,
        current_height: h,
        current_hz,
        available_hz,
    })
}

/// Selects the best mode matching `(width, height)` and `target_hz` within `±1.5 Hz`.
pub fn pick_mode_for_hz(
    modes: &[DisplayModeInfo],
    width: i32,
    height: i32,
    target_hz: u32,
) -> Option<&DisplayModeInfo> {
    let target = target_hz as f64;
    modes
        .iter()
        .filter(|m| {
            m.width == width
                && m.height == height
                && (m.refresh_rate - target).abs() <= 1.5
        })
        .min_by(|a, b| {
            // Prefer non-interlaced, then closest refresh rate, then preferred flag.
            a.is_interlaced
                .cmp(&b.is_interlaced)
                .then_with(|| {
                    (a.refresh_rate - target)
                        .abs()
                        .total_cmp(&(b.refresh_rate - target).abs())
                })
                .then_with(|| b.is_preferred.cmp(&a.is_preferred))
        })
}

fn unwrap_val<'a>(mut val: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(inner) = val {
        val = inner;
    }
    val
}

fn extract_bool(map: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    map.get(key).and_then(|ov| match unwrap_val(ov.deref()) {
        Value::Bool(v) => Some(*v),
        _ => None,
    })
}

fn extract_u32(map: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    map.get(key).and_then(|ov| match unwrap_val(ov.deref()) {
        Value::U32(v) => Some(*v),
        Value::I32(v) if *v >= 0 => Some(*v as u32),
        _ => None,
    })
}

fn mode_from_raw(raw: &RawDisplayMode) -> DisplayModeInfo {
    let (id, width, height, refresh_rate, _pref_scale, _scales, props) = raw;
    DisplayModeInfo {
        id: id.clone(),
        width: *width,
        height: *height,
        refresh_rate: *refresh_rate,
        is_current: extract_bool(props, "is-current").unwrap_or(false),
        is_preferred: extract_bool(props, "is-preferred").unwrap_or(false),
        is_interlaced: extract_bool(props, "is-interlaced").unwrap_or(false),
    }
}

/// Parses `kscreen-doctor -j` JSON output to locate the enabled internal display and its modes.
pub fn parse_kscreen_json(raw: &str) -> Result<KscreenOutputInfo, String> {
    let root: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("Invalid kscreen-doctor JSON: {e}"))?;
    let outputs = root
        .get("outputs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'outputs' array in kscreen-doctor JSON".to_string())?;

    for output in outputs {
        let enabled = output
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        let name = output
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_panel = output
            .get("type")
            .and_then(|v| v.as_i64())
            .is_some_and(|t| t == 14) // KScreen Panel enum if numeric
            || output
                .get("type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t.eq_ignore_ascii_case("panel"));
        if !is_internal_connector(name, is_panel.then_some(true)) {
            continue;
        }

        let current_mode_id = output
            .get("currentModeId")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .unwrap_or_default();
        let preferred_mode_id = output
            .get("preferredModes")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            });

        let Some(raw_modes) = output.get("modes").and_then(|v| v.as_array()) else {
            continue;
        };

        let mut modes = Vec::with_capacity(raw_modes.len());
        for m in raw_modes {
            let Some(id) = m.get("id").and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            }) else {
                continue;
            };
            let size = m.get("size");
            let width = size
                .and_then(|s| s.get("width"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32;
            let height = size
                .and_then(|s| s.get("height"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32;
            let refresh_rate = m
                .get("refreshRate")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if width <= 0 || height <= 0 || refresh_rate <= 0.0 {
                continue;
            }
            modes.push(DisplayModeInfo {
                is_current: id == current_mode_id,
                is_preferred: preferred_mode_id.as_deref() == Some(id.as_str()),
                is_interlaced: false,
                id,
                width,
                height,
                refresh_rate,
            });
        }

        if let Some(info) = summarize_internal_display(name, &modes) {
            return Ok(KscreenOutputInfo { info, modes });
        }
    }

    Err("No enabled internal display found in kscreen-doctor output".to_string())
}

/// Queries the built-in display's current resolution, active Hz, and supported refresh rates.
pub async fn get_internal_display_info() -> Result<InternalDisplayInfo, String> {
    if is_gnome_desktop() {
        let conn = zbus::Connection::session()
            .await
            .map_err(|e| format!("Session D-Bus unavailable: {e}"))?;
        let proxy = MutterDisplayConfigProxy::new(&conn)
            .await
            .map_err(|e| format!("Mutter DisplayConfig proxy unavailable: {e}"))?;
        let (_serial, monitors, _logical, _props) = proxy
            .get_current_state()
            .await
            .map_err(|e| format!("Mutter GetCurrentState failed: {e}"))?;

        for ((connector, _, _, _), raw_modes, monitor_props) in &monitors {
            let is_builtin = extract_bool(monitor_props, "is-builtin");
            if !is_internal_connector(connector, is_builtin) {
                continue;
            }
            let modes: Vec<DisplayModeInfo> = raw_modes.iter().map(mode_from_raw).collect();
            if !modes.iter().any(|m| m.is_current) {
                continue;
            }
            if let Some(info) = summarize_internal_display(connector, &modes) {
                return Ok(info);
            }
        }
        Err("No active internal display found in Mutter DisplayConfig".to_string())
    } else if is_kde_desktop() {
        let out = tokio::process::Command::new("kscreen-doctor")
            .arg("-j")
            .output()
            .await
            .map_err(|e| format!("Failed to run kscreen-doctor: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "kscreen-doctor -j exited with status {}",
                out.status.code().unwrap_or(-1)
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        parse_kscreen_json(&stdout).map(|parsed| parsed.info)
    } else {
        Err("Display refresh rate control requires GNOME or KDE Plasma".to_string())
    }
}

/// Switches the built-in panel's refresh rate to `target_hz` while preserving resolution,
/// scaling, layout, and any external monitors.
pub async fn set_internal_refresh_rate(target_hz: u32) -> Result<u32, String> {
    if is_gnome_desktop() {
        let conn = zbus::Connection::session()
            .await
            .map_err(|e| format!("Session D-Bus unavailable: {e}"))?;
        let proxy = MutterDisplayConfigProxy::new(&conn)
            .await
            .map_err(|e| format!("Mutter DisplayConfig proxy unavailable: {e}"))?;
        let (serial, monitors, logical_monitors, global_props) = proxy
            .get_current_state()
            .await
            .map_err(|e| format!("Mutter GetCurrentState failed: {e}"))?;

        let mut monitor_map: HashMap<&str, (Vec<DisplayModeInfo>, &HashMap<String, OwnedValue>)> =
            HashMap::new();
        let mut internal_connector: Option<String> = None;

        for ((connector, _, _, _), raw_modes, monitor_props) in &monitors {
            let modes: Vec<DisplayModeInfo> = raw_modes.iter().map(mode_from_raw).collect();
            if internal_connector.is_none()
                && is_internal_connector(connector, extract_bool(monitor_props, "is-builtin"))
                && modes.iter().any(|m| m.is_current)
            {
                internal_connector = Some(connector.clone());
            }
            monitor_map.insert(connector.as_str(), (modes, monitor_props));
        }

        let target_connector = internal_connector
            .ok_or_else(|| "No active internal display found in Mutter DisplayConfig".to_string())?;
        let (target_modes, _) = monitor_map
            .get(target_connector.as_str())
            .ok_or_else(|| "Internal display state missing".to_string())?;

        let info = summarize_internal_display(&target_connector, target_modes)
            .ok_or_else(|| "Could not determine internal display mode".to_string())?;
        if info.current_hz == target_hz {
            return Ok(target_hz);
        }

        let target_mode = pick_mode_for_hz(
            target_modes,
            info.current_width,
            info.current_height,
            target_hz,
        )
        .ok_or_else(|| {
            format!(
                "Refresh rate {target_hz} Hz is not supported at {}x{}",
                info.current_width, info.current_height
            )
        })?;
        let target_mode_id = target_mode.id.clone();

        let mut apply_logical: Vec<ApplyLogicalMonitorEntry> =
            Vec::with_capacity(logical_monitors.len());

        for (x, y, scale, transform, primary, attached, _props) in &logical_monitors {
            let mut apply_monitors: Vec<ApplyMonitorEntry> = Vec::with_capacity(attached.len());
            for (connector, _, _, _) in attached {
                let Some((modes, monitor_props)) = monitor_map.get(connector.as_str()) else {
                    return Err(format!("Missing monitor metadata for {connector}"));
                };
                let mode_id = if connector == &target_connector {
                    target_mode_id.clone()
                } else {
                    modes
                        .iter()
                        .find(|m| m.is_current)
                        .or_else(|| modes.iter().find(|m| m.is_preferred))
                        .or_else(|| modes.first())
                        .map(|m| m.id.clone())
                        .ok_or_else(|| format!("No active mode found for {connector}"))?
                };

                let mut opts: HashMap<String, OwnedValue> = HashMap::new();
                if let Some(color_mode) = extract_u32(monitor_props, "color-mode")
                    && let Ok(val) = OwnedValue::try_from(Value::U32(color_mode))
                {
                    opts.insert("color-mode".to_string(), val);
                }
                if let Some(rgb_range) = extract_u32(monitor_props, "rgb-range")
                    && let Ok(val) = OwnedValue::try_from(Value::U32(rgb_range))
                {
                    opts.insert("rgb-range".to_string(), val);
                }

                apply_monitors.push((connector.clone(), mode_id, opts));
            }
            apply_logical.push((*x, *y, *scale, *transform, *primary, apply_monitors));
        }

        let mut apply_props: HashMap<String, OwnedValue> = HashMap::new();
        if extract_bool(&global_props, "supports-changing-layout-mode").unwrap_or(false)
            && let Some(layout_mode) = extract_u32(&global_props, "layout-mode")
            && let Ok(val) = OwnedValue::try_from(Value::U32(layout_mode))
        {
            apply_props.insert("layout-mode".to_string(), val);
        }

        proxy
            .apply_monitors_config(
                serial,
                MUTTER_CONFIG_METHOD_PERSISTENT,
                apply_logical,
                apply_props,
            )
            .await
            .map_err(|e| format!("Mutter ApplyMonitorsConfig failed: {e}"))?;

        Ok(round_hz(target_mode.refresh_rate))
    } else if is_kde_desktop() {
        let out = tokio::process::Command::new("kscreen-doctor")
            .arg("-j")
            .output()
            .await
            .map_err(|e| format!("Failed to run kscreen-doctor: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "kscreen-doctor -j exited with status {}",
                out.status.code().unwrap_or(-1)
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let parsed = parse_kscreen_json(&stdout)?;
        if parsed.info.current_hz == target_hz {
            return Ok(target_hz);
        }
        let mode = pick_mode_for_hz(
            &parsed.modes,
            parsed.info.current_width,
            parsed.info.current_height,
            target_hz,
        )
        .ok_or_else(|| {
            format!(
                "Refresh rate {target_hz} Hz is not supported on {}",
                parsed.info.connector
            )
        })?;

        let arg = format!("output.{}.mode.{}", parsed.info.connector, mode.id);
        run_command_blocking("kscreen-doctor", &[&arg]).await?;
        Ok(round_hz(mode.refresh_rate))
    } else {
        Err("Display refresh rate control requires GNOME or KDE Plasma".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ux3405_modes(current_id: &str) -> Vec<DisplayModeInfo> {
        vec![
            DisplayModeInfo {
                id: "2880x1800@120.000".to_string(),
                width: 2880,
                height: 1800,
                refresh_rate: 119.999,
                is_current: current_id == "2880x1800@120.000",
                is_preferred: true,
                is_interlaced: false,
            },
            DisplayModeInfo {
                id: "2880x1800@60.000".to_string(),
                width: 2880,
                height: 1800,
                refresh_rate: 60.001,
                is_current: current_id == "2880x1800@60.000",
                is_preferred: false,
                is_interlaced: false,
            },
            DisplayModeInfo {
                id: "1920x1200@60.000".to_string(),
                width: 1920,
                height: 1200,
                refresh_rate: 60.0,
                is_current: false,
                is_preferred: false,
                is_interlaced: false,
            },
        ]
    }

    #[test]
    fn test_round_hz_and_internal_connector() {
        assert_eq!(round_hz(59.999), 60);
        assert_eq!(round_hz(119.88), 120);
        assert_eq!(round_hz(0.0), 0);
        assert_eq!(round_hz(-10.0), 0);
        assert_eq!(round_hz(f64::NAN), 0);

        assert!(is_internal_connector("eDP-1", None));
        assert!(is_internal_connector("eDP-2", Some(false)));
        assert!(is_internal_connector("DP-3", Some(true)));
        assert!(!is_internal_connector("HDMI-A-1", Some(false)));
        assert!(!is_internal_connector("DP-1", None));
    }

    #[test]
    fn test_summarize_and_pick_mode_ux3405ca() {
        let modes = ux3405_modes("2880x1800@120.000");
        let summary = summarize_internal_display("eDP-1", &modes).expect("summary");
        assert_eq!(summary.connector, "eDP-1");
        assert_eq!(summary.current_width, 2880);
        assert_eq!(summary.current_height, 1800);
        assert_eq!(summary.current_hz, 120);
        assert_eq!(summary.available_hz, vec![60, 120]);

        let picked_60 = pick_mode_for_hz(&modes, 2880, 1800, 60).expect("60hz mode");
        assert_eq!(picked_60.id, "2880x1800@60.000");

        let picked_120 = pick_mode_for_hz(&modes, 2880, 1800, 120).expect("120hz mode");
        assert_eq!(picked_120.id, "2880x1800@120.000");

        assert!(pick_mode_for_hz(&modes, 2880, 1800, 144).is_none());
    }

    #[test]
    fn test_pick_mode_prefers_progressive_over_interlaced() {
        let modes = vec![
            DisplayModeInfo {
                id: "interlaced".to_string(),
                width: 1920,
                height: 1080,
                refresh_rate: 60.0,
                is_current: false,
                is_preferred: false,
                is_interlaced: true,
            },
            DisplayModeInfo {
                id: "progressive".to_string(),
                width: 1920,
                height: 1080,
                refresh_rate: 59.94,
                is_current: true,
                is_preferred: true,
                is_interlaced: false,
            },
        ];

        let summary = summarize_internal_display("eDP-1", &modes).unwrap();
        assert_eq!(summary.available_hz, vec![60]);
        assert_eq!(
            pick_mode_for_hz(&modes, 1920, 1080, 60).unwrap().id,
            "progressive"
        );
    }

    #[test]
    fn test_parse_kscreen_json() {
        let json = r#"{
            "outputs": [
                {
                    "name": "HDMI-A-1",
                    "enabled": true,
                    "type": "HDMI",
                    "currentModeId": "1",
                    "modes": [
                        { "id": "1", "refreshRate": 60.0, "size": { "width": 1920, "height": 1080 } }
                    ]
                },
                {
                    "name": "eDP-1",
                    "enabled": true,
                    "type": "Panel",
                    "currentModeId": "2",
                    "preferredModes": ["1"],
                    "modes": [
                        { "id": "1", "refreshRate": 120.0, "size": { "width": 2880, "height": 1800 } },
                        { "id": "2", "refreshRate": 60.0, "size": { "width": 2880, "height": 1800 } }
                    ]
                }
            ]
        }"#;

        let parsed = parse_kscreen_json(json).expect("valid kscreen json");
        assert_eq!(parsed.info.connector, "eDP-1");
        assert_eq!(parsed.info.current_hz, 60);
        assert_eq!(parsed.info.available_hz, vec![60, 120]);
        assert_eq!(
            pick_mode_for_hz(&parsed.modes, 2880, 1800, 120)
                .unwrap()
                .id,
            "1"
        );
    }

    #[test]
    fn test_parse_kscreen_json_errors_on_no_internal() {
        let json = r#"{ "outputs": [] }"#;
        assert!(parse_kscreen_json(json).is_err());
        assert!(parse_kscreen_json("not json").is_err());
    }
}
