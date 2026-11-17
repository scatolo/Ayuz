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

use std::path::{Path, PathBuf};

pub const SYS_PRODUCT_NAME: &str = "/sys/class/dmi/id/product_name";
pub const SYS_BOARD_NAME: &str = "/sys/class/dmi/id/board_name";
pub const SYS_BIOS_VERSION: &str = "/sys/class/dmi/id/bios_version";
pub const SYS_BIOS_DATE: &str = "/sys/class/dmi/id/bios_date";
pub const SYS_PRODUCT_SERIAL: &str = "/sys/class/dmi/id/product_serial";
pub const SYS_POWER_SUPPLY_DIR: &str = "/sys/class/power_supply";
pub const SYS_PROC_STAT: &str = "/proc/stat";
pub const SYS_MEM_INFO: &str = "/proc/meminfo";
pub const SYS_HWMON_DIR: &str = "/sys/class/hwmon";
pub const SYS_THERMAL_ZONE0_TEMP: &str = "/sys/class/thermal/thermal_zone0/temp";
pub const SYS_MEM_SLEEP: &str = "/sys/power/mem_sleep";
pub const SYS_CPU_EPP: &str = "/sys/devices/system/cpu/cpufreq/policy0/energy_performance_preference";
pub const SYS_CPU_GOVERNOR: &str = "/sys/devices/system/cpu/cpufreq/policy0/scaling_governor";
pub const SYS_INTEL_NO_TURBO: &str = "/sys/devices/system/cpu/intel_pstate/no_turbo";
pub const SYS_INTEL_MAX_PERF_PCT: &str = "/sys/devices/system/cpu/intel_pstate/max_perf_pct";
pub const SYS_INTEL_MIN_PERF_PCT: &str = "/sys/devices/system/cpu/intel_pstate/min_perf_pct";
pub const MIN_SAFE_PERF_PCT: u8 = 30;
pub const MAX_SAFE_PERF_PCT: u8 = 100;
pub const SYS_ASUS_ARMOURY_ATTRS_DIR: &str =
    "/sys/class/firmware-attributes/asus-armoury/attributes";
pub const SYS_DRM_DIR: &str = "/sys/class/drm";
pub const SYS_BACKLIGHT_DIR: &str = "/sys/class/backlight";
pub const PROC_ACPI_LID_DIR: &str = "/proc/acpi/button/lid";
pub const DEV_UINPUT: &str = "/dev/uinput";
pub const DEV_INPUT_DIR: &str = "/dev/input";
pub const PROC_BUS_INPUT_DEVICES: &str = "/proc/bus/input/devices";

/// Resolves the directory where AniMatrix GIF assets are stored.
///
/// Resolution order:
/// 1. `CARGO_MANIFEST_DIR/assets/anime` - cargo dev runs
/// 2. `$APPDIR/assets/anime` - inside a running AppImage
/// 3. `<binary_parent>/../share/ayuz/anime` - installed via deb/rpm (e.g. `/usr/share/ayuz/anime`)
pub fn anime_assets_dir() -> PathBuf {
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        return PathBuf::from(manifest).join("assets/anime");
    }
    if let Ok(appdir) = std::env::var("APPDIR") {
        return PathBuf::from(appdir).join("assets/anime");
    }
    std::env::current_exe()
        .unwrap_or_default()
        .parent()
        .unwrap_or(Path::new("/usr/bin"))
        .parent()
        .unwrap_or(Path::new("/usr"))
        .join("share/ayuz/anime")
}

/// Dynamically locates all system battery directories under `base_dir`.
/// Scans entries for a `type` file containing "Battery" (case-insensitive)
/// and skips peripheral batteries whose `scope` file is "Device" (case-insensitive).
pub fn find_battery_dirs_in(base_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return Vec::new();
    };

    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    dirs.sort();

    let mut result = Vec::new();
    for dir in dirs {
        let Ok(content) = std::fs::read_to_string(dir.join("type")) else {
            continue;
        };
        if !content.trim().eq_ignore_ascii_case("battery") {
            continue;
        }
        if std::fs::read_to_string(dir.join("scope"))
            .is_ok_and(|scope| scope.trim().eq_ignore_ascii_case("device"))
        {
            continue;
        }
        result.push(dir);
    }

    result
}

/// Dynamically locates all battery capacity sysfs paths under `SYS_POWER_SUPPLY_DIR`.
#[allow(dead_code)]
pub fn find_battery_capacity_paths() -> Vec<PathBuf> {
    find_battery_capacity_paths_in(Path::new(SYS_POWER_SUPPLY_DIR))
}

/// Dynamically locates all battery capacity sysfs paths under `base_dir`.
/// Scans entries for a `type` file containing "Battery" (case-insensitive),
/// skips peripheral batteries whose `scope` file is "Device" (case-insensitive),
/// and returns the path to `capacity` if present.
#[allow(dead_code)]
pub fn find_battery_capacity_paths_in(base_dir: &Path) -> Vec<PathBuf> {
    find_battery_dirs_in(base_dir)
        .into_iter()
        .map(|dir| dir.join("capacity"))
        .filter(|cap_path| cap_path.exists())
        .collect()
}

/// Dynamically locates the CPU temperature sysfs path from `/sys/class/hwmon`,
/// falling back to `/sys/class/thermal/thermal_zone0/temp`.
pub fn find_cpu_temp_path() -> PathBuf {
    find_cpu_temp_path_in(
        Path::new(SYS_HWMON_DIR),
        Path::new(SYS_THERMAL_ZONE0_TEMP),
    )
}

/// Dynamically locates the CPU temperature sysfs path under `hwmon_dir`.
/// Scans for hwmon entries whose `name` file matches known CPU drivers
/// (`coretemp`, `k10temp`, `zenpower`, or containing `cpu`).
/// Prefers `temp1_input`, falling back to any `temp*_input`.
/// If no CPU hwmon sensor is found, returns `thermal_fallback_path`.
pub fn find_cpu_temp_path_in(hwmon_dir: &Path, thermal_fallback_path: &Path) -> PathBuf {
    let Ok(entries) = std::fs::read_dir(hwmon_dir) else {
        return thermal_fallback_path.to_path_buf();
    };

    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    dirs.sort();

    for dir in dirs {
        let name_path = dir.join("name");
        let Ok(name) = std::fs::read_to_string(&name_path) else {
            continue;
        };
        let name = name.trim().to_lowercase();
        if name == "coretemp" || name == "k10temp" || name == "zenpower" || name.contains("cpu") {
            let temp1 = dir.join("temp1_input");
            if temp1.exists() {
                return temp1;
            }

            if let Ok(sub_entries) = std::fs::read_dir(&dir) {
                let mut temp_inputs: Vec<PathBuf> = sub_entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.starts_with("temp") && n.ends_with("_input"))
                            .unwrap_or(false)
                    })
                    .collect();
                temp_inputs.sort();
                if let Some(first) = temp_inputs.into_iter().next() {
                    return first;
                }
            }
        }
    }

    thermal_fallback_path.to_path_buf()
}

/// Parses a raw sysfs battery capacity string into a percentage clamped to `0..=100`.
pub fn parse_battery_capacity(raw: &str) -> Option<u32> {
    raw.trim().parse::<u32>().ok().map(|v| v.min(100))
}

/// Aggregates a slice of battery capacity percentages into an average percentage string,
/// clamping each entry to `100`. Returns "N/A" if the slice is empty.
#[allow(dead_code)]
pub fn aggregate_battery_percentage(capacities: &[u32]) -> String {
    if capacities.is_empty() {
        "N/A".to_string()
    } else {
        let avg = capacities.iter().map(|&c| c.min(100)).sum::<u32>() / capacities.len() as u32;
        format!("{}%", avg)
    }
}

/// Formats a raw millidegree Celsius string into a whole degrees string with unit ("<deg>°C").
pub fn format_cpu_temp(raw_millideg: &str) -> String {
    let millideg: i32 = raw_millideg.trim().parse().unwrap_or(0);
    format!("{}°C", millideg / 1000)
}

/// Snapshot of cumulative CPU jiffies parsed from the aggregate `cpu` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuStatSample {
    pub total: u64,
    pub idle_all: u64,
}

/// Parses the aggregate `cpu` row of `/proc/stat` into cumulative total and idle jiffies.
///
/// Linux `/proc/stat` columns are: `user nice system idle iowait irq softirq steal [guest guest_nice]`.
/// `guest` and `guest_nice` are already accounted for in `user` and `nice` by the kernel and are
/// intentionally excluded so they are not double-counted.
pub fn parse_proc_stat(raw: &str) -> Option<CpuStatSample> {
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("cpu") {
            continue;
        }
        let user: u64 = parts.next()?.parse().ok()?;
        let nice: u64 = parts.next()?.parse().ok()?;
        let system: u64 = parts.next()?.parse().ok()?;
        let idle: u64 = parts.next()?.parse().ok()?;

        let mut opt = || -> Option<u64> {
            match parts.next() {
                Some(s) => s.parse().ok(),
                None => Some(0),
            }
        };
        let iowait = opt()?;
        let irq = opt()?;
        let softirq = opt()?;
        let steal = opt()?;

        let idle_all = idle.saturating_add(iowait);
        let non_idle = user
            .saturating_add(nice)
            .saturating_add(system)
            .saturating_add(irq)
            .saturating_add(softirq)
            .saturating_add(steal);
        let total = idle_all.saturating_add(non_idle);
        return Some(CpuStatSample { total, idle_all });
    }
    None
}

/// Computes integer CPU utilization percentage (`0..=100`) between two `/proc/stat` samples.
///
/// Returns `None` when total jiffies did not advance (`curr.total <= prev.total`).
pub fn calculate_cpu_usage_percent(prev: CpuStatSample, curr: CpuStatSample) -> Option<u32> {
    if curr.total <= prev.total {
        return None;
    }
    let total_delta = curr.total - prev.total;
    let idle_delta = curr.idle_all.saturating_sub(prev.idle_all);
    let active_delta = total_delta.saturating_sub(idle_delta);
    let pct = ((active_delta as f64 / total_delta as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u32;
    Some(pct)
}

/// Aggregated battery telemetry read from `/sys/class/power_supply/BAT*`.
#[derive(Debug, Clone, PartialEq)]
pub struct BatteryTelemetry {
    pub capacity_percent: Option<u32>,
    pub status: Option<String>,
    pub power_watts: Option<f64>,
    pub health_percent: Option<f64>,
    pub cycle_count: Option<u32>,
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_trimmed(path)?.parse::<u64>().ok()
}

fn read_i64(path: &Path) -> Option<i64> {
    read_trimmed(path)?.parse::<i64>().ok()
}

/// Parses battery telemetry from a single `/sys/class/power_supply/BAT*` directory.
pub fn parse_battery_dir(dir: &Path) -> BatteryTelemetry {
    let capacity_percent = read_trimmed(&dir.join("capacity")).and_then(|s| parse_battery_capacity(&s));
    let status = read_trimmed(&dir.join("status"));

    let power_watts = if let Some(pw_uw) = read_i64(&dir.join("power_now")) {
        Some(pw_uw.unsigned_abs() as f64 / 1_000_000.0)
    } else if let (Some(curr_ua), Some(volt_uv)) = (
        read_i64(&dir.join("current_now")),
        read_u64(&dir.join("voltage_now")),
    ) {
        Some((curr_ua.unsigned_abs() as f64 * volt_uv as f64) / 1_000_000_000_000.0)
    } else {
        None
    };

    let health_percent = if let (Some(full), Some(design)) = (
        read_u64(&dir.join("energy_full")),
        read_u64(&dir.join("energy_full_design")),
    ) && design > 0
    {
        Some(((full as f64 / design as f64) * 100.0).clamp(0.0, 100.0))
    } else if let (Some(full), Some(design)) = (
        read_u64(&dir.join("charge_full")),
        read_u64(&dir.join("charge_full_design")),
    ) && design > 0
    {
        Some(((full as f64 / design as f64) * 100.0).clamp(0.0, 100.0))
    } else {
        None
    };

    let cycle_count = read_trimmed(&dir.join("cycle_count")).and_then(|s| s.parse::<u32>().ok());

    BatteryTelemetry {
        capacity_percent,
        status,
        power_watts,
        health_percent,
        cycle_count,
    }
}

/// Reads and aggregates telemetry for all system batteries under `SYS_POWER_SUPPLY_DIR`.
pub fn read_battery_telemetry() -> Option<BatteryTelemetry> {
    read_battery_telemetry_in(Path::new(SYS_POWER_SUPPLY_DIR))
}

/// Reads and aggregates telemetry for all system batteries under `base_dir`.
pub fn read_battery_telemetry_in(base_dir: &Path) -> Option<BatteryTelemetry> {
    let dirs = find_battery_dirs_in(base_dir);
    if dirs.is_empty() {
        return None;
    }

    let entries: Vec<BatteryTelemetry> = dirs.iter().map(|d| parse_battery_dir(d)).collect();

    let caps: Vec<u32> = entries.iter().filter_map(|e| e.capacity_percent).collect();
    let capacity_percent = if caps.is_empty() {
        None
    } else {
        Some(caps.iter().sum::<u32>() / caps.len() as u32)
    };

    let status = entries
        .iter()
        .filter_map(|e| e.status.as_ref())
        .find(|s| !s.eq_ignore_ascii_case("unknown"))
        .cloned()
        .or_else(|| entries.iter().find_map(|e| e.status.clone()));

    let watts: Vec<f64> = entries.iter().filter_map(|e| e.power_watts).collect();
    let power_watts = if watts.is_empty() {
        None
    } else {
        Some(watts.iter().sum())
    };

    let healths: Vec<f64> = entries.iter().filter_map(|e| e.health_percent).collect();
    let health_percent = if healths.is_empty() {
        None
    } else {
        Some(healths.iter().sum::<f64>() / healths.len() as f64)
    };

    let cycle_count = entries.iter().find_map(|e| e.cycle_count);

    Some(BatteryTelemetry {
        capacity_percent,
        status,
        power_watts,
        health_percent,
        cycle_count,
    })
}

/// Read-only CPU power/EPP telemetry from `cpufreq` / `intel_pstate`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuPowerTelemetry {
    pub epp: Option<String>,
    pub governor: Option<String>,
    pub turbo_enabled: Option<bool>,
    pub max_perf_pct: Option<u8>,
    pub min_perf_pct: Option<u8>,
    pub pstate_writable: bool,
}

/// Parses `/sys/devices/system/cpu/intel_pstate/no_turbo` (`"0"` = enabled, `"1"` = disabled).
pub fn parse_no_turbo(raw: &str) -> Option<bool> {
    match raw.trim() {
        "0" => Some(true),
        "1" => Some(false),
        _ => None,
    }
}

/// Parses an `intel_pstate` `max_perf_pct` / `min_perf_pct` sysfs value (`0..=100`).
pub fn parse_perf_pct(raw: &str) -> Option<u8> {
    raw.trim().parse::<u8>().ok().filter(|&v| v <= 100)
}

/// Clamps an `intel_pstate` performance percentage to the safe `30..=100` guard range
/// so the CPU cannot accidentally be pinned to unusable ultra-low frequencies (e.g. `8%`).
pub fn clamp_perf_pct(val: u8) -> u8 {
    val.clamp(MIN_SAFE_PERF_PCT, MAX_SAFE_PERF_PCT)
}

/// Reads read-only CPU EPP, governor, and `intel_pstate` telemetry from the live sysfs paths.
pub fn read_cpu_power_telemetry() -> CpuPowerTelemetry {
    read_cpu_power_telemetry_in(
        Path::new(SYS_CPU_EPP),
        Path::new(SYS_CPU_GOVERNOR),
        Path::new(SYS_INTEL_NO_TURBO),
        Path::new(SYS_INTEL_MAX_PERF_PCT),
        Path::new(SYS_INTEL_MIN_PERF_PCT),
    )
}

/// Reads CPU power telemetry from arbitrary paths (for headless unit testing).
pub fn read_cpu_power_telemetry_in(
    epp_path: &Path,
    governor_path: &Path,
    no_turbo_path: &Path,
    max_perf_pct_path: &Path,
    min_perf_pct_path: &Path,
) -> CpuPowerTelemetry {
    CpuPowerTelemetry {
        epp: read_trimmed(epp_path),
        governor: read_trimmed(governor_path),
        turbo_enabled: read_trimmed(no_turbo_path).and_then(|s| parse_no_turbo(&s)),
        max_perf_pct: read_trimmed(max_perf_pct_path).and_then(|s| parse_perf_pct(&s)),
        min_perf_pct: read_trimmed(min_perf_pct_path).and_then(|s| parse_perf_pct(&s)),
        pstate_writable: is_path_writable(max_perf_pct_path),
    }
}

/// Checks whether an `asus-armoury` firmware attribute's `current_value` file has owner write permission.
///
/// Attributes like `charge_mode` on UX3405CA are exposed read-only (`0444`) by the kernel driver;
/// returning `false` lets the UI render them as read-only info rows instead of broken controls.
pub fn is_armoury_attr_writable(attr_name: &str) -> bool {
    is_armoury_attr_writable_in(Path::new(SYS_ASUS_ARMOURY_ATTRS_DIR), attr_name)
}

/// Path-parameterized version of [`is_armoury_attr_writable`] for unit testing.
pub fn is_armoury_attr_writable_in(base_dir: &Path, attr_name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let path = base_dir.join(attr_name).join("current_value");
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o200 != 0)
        .unwrap_or(false)
}

/// Checks `/sys/class/drm/card*/device/vendor` to detect whether a discrete GPU
/// (NVIDIA `0x10de` or multiple PCI GPU cards) is present.
///
/// Returns `Some(false)` when only a single integrated GPU is detected (e.g. Intel
/// `0x8086` on UX3405CA), `Some(true)` if a discrete/secondary GPU is found, and
/// `None` if `/sys/class/drm` exposes no readable card vendor entries.
pub fn has_discrete_gpu() -> Option<bool> {
    has_discrete_gpu_in(Path::new(SYS_DRM_DIR))
}

/// Path-parameterized version of [`has_discrete_gpu`] for unit testing.
pub fn has_discrete_gpu_in(drm_dir: &Path) -> Option<bool> {
    let entries = std::fs::read_dir(drm_dir).ok()?;
    let mut vendors = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("card") || name_str.contains('-') {
            continue;
        }
        if let Some(vendor) = read_trimmed(&entry.path().join("device").join("vendor")) {
            vendors.push(vendor.to_ascii_lowercase());
        }
    }

    if vendors.is_empty() {
        return None;
    }
    if vendors.iter().any(|v| v == "0x10de") || vendors.len() >= 2 {
        Some(true)
    } else {
        Some(false)
    }
}

/// Checks whether a whitespace-delimited group list (e.g. `id -Gn`) contains `target`.
pub fn parse_groups_contains(raw_groups: &str, target: &str) -> bool {
    raw_groups.split_whitespace().any(|g| g == target)
}

/// Returns `true` if the current process belongs to the `input` group.
pub fn has_input_group() -> bool {
    std::process::Command::new("id")
        .arg("-Gn")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_groups_contains(&String::from_utf8_lossy(&o.stdout), "input"))
        .unwrap_or(false)
}

/// Returns `true` if at least one `/dev/input/event*` node is readable by this process.
pub fn can_read_any_input_event() -> bool {
    can_read_any_input_event_in(Path::new(DEV_INPUT_DIR))
}

/// Path-parameterized version of [`can_read_any_input_event`] for unit testing.
pub fn can_read_any_input_event_in(input_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(input_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("event") {
            continue;
        }
        if std::fs::File::open(entry.path()).is_ok() {
            return true;
        }
    }
    false
}

/// Returns `true` if `path` exists and can be opened for writing without blocking.
pub fn is_path_writable(path: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .is_ok()
}

/// Returns `true` if at least one connected display appears to be powered on according to
/// `/sys/class/drm` and `/sys/class/backlight`.
///
/// Fails open (`true`) when sysfs exposes neither DRM connector state nor backlight power state
/// (e.g. inside a headless container).
pub fn is_any_display_powered() -> bool {
    is_any_display_powered_in(Path::new(SYS_DRM_DIR), Path::new(SYS_BACKLIGHT_DIR))
}

/// Path-parameterized version of [`is_any_display_powered`] for headless unit testing.
pub fn is_any_display_powered_in(drm_dir: &Path, backlight_dir: &Path) -> bool {
    let mut bl_any_found = false;
    let mut bl_any_on = false;
    if let Ok(entries) = std::fs::read_dir(backlight_dir) {
        for entry in entries.flatten() {
            if let Some(bl_power) = read_trimmed(&entry.path().join("bl_power")) {
                bl_any_found = true;
                if bl_power == "0" {
                    bl_any_on = true;
                }
            }
        }
    }
    let internal_bl_powered_off = bl_any_found && !bl_any_on;

    let mut connected_count = 0usize;
    let mut active_count = 0usize;
    if let Ok(entries) = std::fs::read_dir(drm_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("card") || !name_str.contains('-') {
                continue;
            }
            let path = entry.path();
            let Some(status) = read_trimmed(&path.join("status")) else {
                continue;
            };
            if !status.eq_ignore_ascii_case("connected") {
                continue;
            }
            connected_count += 1;

            let enabled_ok = read_trimmed(&path.join("enabled"))
                .map(|v| !v.eq_ignore_ascii_case("disabled"))
                .unwrap_or(true);
            let dpms_ok = read_trimmed(&path.join("dpms"))
                .map(|v| v.eq_ignore_ascii_case("on"))
                .unwrap_or(true);

            let lower_name = name_str.to_ascii_lowercase();
            let is_internal = lower_name.contains("-edp-")
                || lower_name.contains("-lvds-")
                || lower_name.contains("-dsi-");
            let bl_ok = !(is_internal && internal_bl_powered_off);

            if enabled_ok && dpms_ok && bl_ok {
                active_count += 1;
            }
        }
    }

    if connected_count > 0 {
        active_count > 0
    } else if bl_any_found {
        bl_any_on
    } else {
        true
    }
}

/// Returns `true` if `/proc/acpi/button/lid/*/state` reports the laptop lid as closed.
pub fn is_lid_closed() -> bool {
    is_lid_closed_in(Path::new(PROC_ACPI_LID_DIR))
}

/// Path-parameterized version of [`is_lid_closed`] for headless unit testing.
pub fn is_lid_closed_in(lid_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(lid_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if let Some(state) = read_trimmed(&entry.path().join("state"))
            && state.to_ascii_lowercase().contains("closed")
        {
            return true;
        }
    }
    false
}

/// Formats an OS-appropriate package install command based on `/etc/os-release`.
pub fn package_install_command(pkg_fedora: &str, pkg_debian: &str, pkg_arch: &str) -> String {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    package_install_command_from_os_release(&os_release, pkg_fedora, pkg_debian, pkg_arch)
}

/// Pure helper to format an install command from `/etc/os-release` contents.
pub fn package_install_command_from_os_release(
    os_release: &str,
    pkg_fedora: &str,
    pkg_debian: &str,
    pkg_arch: &str,
) -> String {
    let lower = os_release.to_ascii_lowercase();
    if lower.contains("fedora") || lower.contains("rhel") || lower.contains("centos") {
        format!("sudo dnf install -y {pkg_fedora}")
    } else if lower.contains("ubuntu") || lower.contains("debian") || lower.contains("pop") {
        format!("sudo apt install -y {pkg_debian}")
    } else if lower.contains("arch") || lower.contains("cachyos") || lower.contains("manjaro") {
        format!("sudo pacman -S {pkg_arch}")
    } else {
        format!("sudo dnf install -y {pkg_fedora}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempMockDir {
        path: PathBuf,
    }

    impl TempMockDir {
        fn new(prefix: &str) -> Self {
            let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
            let unique_name = format!("ayuz_test_{}_{}_{}", prefix, std::process::id(), count);
            let path = std::env::temp_dir().join(unique_name);
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("Failed to create mock directory");
            Self { path }
        }
    }

    impl Drop for TempMockDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    // --- Battery Detection Tests ---

    #[test]
    fn test_battery_empty_dir() {
        let mock = TempMockDir::new("battery_empty");
        let paths = find_battery_capacity_paths_in(&mock.path);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_battery_nonexistent_dir() {
        let nonexistent = std::env::temp_dir().join("ayuz_nonexistent_dir_12345");
        let paths = find_battery_capacity_paths_in(&nonexistent);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_battery_single_battery() {
        let mock = TempMockDir::new("battery_single");
        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();
        fs::write(bat0.join("capacity"), "80\n").unwrap();

        let paths = find_battery_capacity_paths_in(&mock.path);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], bat0.join("capacity"));
    }

    #[test]
    fn test_battery_multiple_batteries() {
        let mock = TempMockDir::new("battery_multi");
        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();
        fs::write(bat0.join("capacity"), "80\n").unwrap();

        let bat1 = mock.path.join("BAT1");
        fs::create_dir_all(&bat1).unwrap();
        fs::write(bat1.join("type"), "Battery\n").unwrap();
        fs::write(bat1.join("capacity"), "60\n").unwrap();

        let paths = find_battery_capacity_paths_in(&mock.path);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], bat0.join("capacity"));
        assert_eq!(paths[1], bat1.join("capacity"));
    }

    #[test]
    fn test_battery_ignores_mains_and_usb() {
        let mock = TempMockDir::new("battery_filter");
        let ac0 = mock.path.join("AC0");
        fs::create_dir_all(&ac0).unwrap();
        fs::write(ac0.join("type"), "Mains\n").unwrap();

        let usb = mock.path.join("USBC000");
        fs::create_dir_all(&usb).unwrap();
        fs::write(usb.join("type"), "USB\n").unwrap();

        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();
        fs::write(bat0.join("capacity"), "92\n").unwrap();

        let paths = find_battery_capacity_paths_in(&mock.path);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], bat0.join("capacity"));
    }

    #[test]
    fn test_battery_case_insensitive_and_whitespace() {
        let mock = TempMockDir::new("battery_case");
        let batc = mock.path.join("BATC");
        fs::create_dir_all(&batc).unwrap();
        fs::write(batc.join("type"), "  bAtTeRy \r\n").unwrap();
        fs::write(batc.join("capacity"), "75\n").unwrap();

        let paths = find_battery_capacity_paths_in(&mock.path);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], batc.join("capacity"));
    }

    #[test]
    fn test_battery_missing_capacity_file() {
        let mock = TempMockDir::new("battery_no_cap");
        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();

        let paths = find_battery_capacity_paths_in(&mock.path);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_battery_ignores_device_scope() {
        let mock = TempMockDir::new("battery_scope");

        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();
        fs::write(bat0.join("scope"), "System\n").unwrap();
        fs::write(bat0.join("capacity"), "80\n").unwrap();

        let bat1 = mock.path.join("BAT1");
        fs::create_dir_all(&bat1).unwrap();
        fs::write(bat1.join("type"), "Battery\n").unwrap();
        fs::write(bat1.join("capacity"), "60\n").unwrap();

        let bat2 = mock.path.join("BAT2");
        fs::create_dir_all(&bat2).unwrap();
        fs::write(bat2.join("type"), "Battery\n").unwrap();
        fs::write(bat2.join("scope"), "Unknown\n").unwrap();
        fs::write(bat2.join("capacity"), "70\n").unwrap();

        let mouse = mock.path.join("hidpp_battery_0");
        fs::create_dir_all(&mouse).unwrap();
        fs::write(mouse.join("type"), "Battery\n").unwrap();
        fs::write(mouse.join("scope"), "Device\n").unwrap();
        fs::write(mouse.join("capacity"), "50\n").unwrap();

        let headset = mock.path.join("headset_battery");
        fs::create_dir_all(&headset).unwrap();
        fs::write(headset.join("type"), "Battery\n").unwrap();
        fs::write(headset.join("scope"), "  device \r\n").unwrap();
        fs::write(headset.join("capacity"), "30\n").unwrap();

        let paths = find_battery_capacity_paths_in(&mock.path);
        assert_eq!(
            paths,
            vec![
                bat0.join("capacity"),
                bat1.join("capacity"),
                bat2.join("capacity"),
            ]
        );
    }

    // --- CPU Temperature Detection Tests ---

    #[test]
    fn test_cpu_temp_nonexistent_hwmon() {
        let nonexistent = std::env::temp_dir().join("ayuz_hwmon_nonexistent_12345");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");
        let path = find_cpu_temp_path_in(&nonexistent, &fallback);
        assert_eq!(path, fallback);
    }

    #[test]
    fn test_cpu_temp_empty_hwmon() {
        let mock = TempMockDir::new("hwmon_empty");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");
        let path = find_cpu_temp_path_in(&mock.path, &fallback);
        assert_eq!(path, fallback);
    }

    #[test]
    fn test_cpu_temp_ignores_non_cpu() {
        let mock = TempMockDir::new("hwmon_non_cpu");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");

        let hwmon0 = mock.path.join("hwmon0");
        fs::create_dir_all(&hwmon0).unwrap();
        fs::write(hwmon0.join("name"), "nvme\n").unwrap();
        fs::write(hwmon0.join("temp1_input"), "35000\n").unwrap();

        let hwmon1 = mock.path.join("hwmon1");
        fs::create_dir_all(&hwmon1).unwrap();
        fs::write(hwmon1.join("name"), "asus\n").unwrap();
        fs::write(hwmon1.join("temp1_input"), "40000\n").unwrap();

        let path = find_cpu_temp_path_in(&mock.path, &fallback);
        assert_eq!(path, fallback);
    }

    #[test]
    fn test_cpu_temp_intel_coretemp() {
        let mock = TempMockDir::new("hwmon_intel");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");

        let hwmon8 = mock.path.join("hwmon8");
        fs::create_dir_all(&hwmon8).unwrap();
        fs::write(hwmon8.join("name"), "coretemp\n").unwrap();
        fs::write(hwmon8.join("temp1_input"), "49000\n").unwrap();

        let path = find_cpu_temp_path_in(&mock.path, &fallback);
        assert_eq!(path, hwmon8.join("temp1_input"));
    }

    #[test]
    fn test_cpu_temp_amd_k10temp() {
        let mock = TempMockDir::new("hwmon_amd_k10");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");

        let hwmon1 = mock.path.join("hwmon1");
        fs::create_dir_all(&hwmon1).unwrap();
        fs::write(hwmon1.join("name"), "k10temp\n").unwrap();
        fs::write(hwmon1.join("temp1_input"), "52000\n").unwrap();

        let path = find_cpu_temp_path_in(&mock.path, &fallback);
        assert_eq!(path, hwmon1.join("temp1_input"));
    }

    #[test]
    fn test_cpu_temp_amd_zenpower() {
        let mock = TempMockDir::new("hwmon_amd_zen");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");

        let hwmon2 = mock.path.join("hwmon2");
        fs::create_dir_all(&hwmon2).unwrap();
        fs::write(hwmon2.join("name"), "zenpower\n").unwrap();
        fs::write(hwmon2.join("temp1_input"), "48000\n").unwrap();

        let path = find_cpu_temp_path_in(&mock.path, &fallback);
        assert_eq!(path, hwmon2.join("temp1_input"));
    }

    #[test]
    fn test_cpu_temp_arm_soc_cpu() {
        let mock = TempMockDir::new("hwmon_arm");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");

        let hwmon0 = mock.path.join("hwmon0");
        fs::create_dir_all(&hwmon0).unwrap();
        fs::write(hwmon0.join("name"), "soc_cpu_thermal\n").unwrap();
        fs::write(hwmon0.join("temp1_input"), "45000\n").unwrap();

        let path = find_cpu_temp_path_in(&mock.path, &fallback);
        assert_eq!(path, hwmon0.join("temp1_input"));
    }

    #[test]
    fn test_cpu_temp_fallback_to_temp2() {
        let mock = TempMockDir::new("hwmon_temp2");
        let fallback = PathBuf::from("/fallback/thermal_zone0/temp");

        let hwmon1 = mock.path.join("hwmon1");
        fs::create_dir_all(&hwmon1).unwrap();
        fs::write(hwmon1.join("name"), "coretemp\n").unwrap();
        // temp1_input omitted, temp2_input present
        fs::write(hwmon1.join("temp2_input"), "47000\n").unwrap();

        let path = find_cpu_temp_path_in(&mock.path, &fallback);
        assert_eq!(path, hwmon1.join("temp2_input"));
    }

    // --- Pure Calculation Helpers Tests ---

    #[test]
    fn test_parse_battery_capacity() {
        assert_eq!(parse_battery_capacity("85\n"), Some(85));
        assert_eq!(parse_battery_capacity(" 100 \r\n"), Some(100));
        assert_eq!(parse_battery_capacity("105\n"), Some(100));
        assert_eq!(parse_battery_capacity(&u32::MAX.to_string()), Some(100));
        assert_eq!(parse_battery_capacity("0"), Some(0));
        assert_eq!(parse_battery_capacity("-5"), None);
        assert_eq!(parse_battery_capacity("invalid"), None);
        assert_eq!(parse_battery_capacity(""), None);
    }

    #[test]
    fn test_aggregate_battery_percentage() {
        assert_eq!(aggregate_battery_percentage(&[]), "N/A");
        assert_eq!(aggregate_battery_percentage(&[85]), "85%");
        assert_eq!(aggregate_battery_percentage(&[80, 60]), "70%");
        assert_eq!(aggregate_battery_percentage(&[0]), "0%");
        assert_eq!(aggregate_battery_percentage(&[105]), "100%");
        assert_eq!(aggregate_battery_percentage(&[150, 80]), "90%");
        assert_eq!(aggregate_battery_percentage(&[u32::MAX, 50]), "75%");
    }

    #[test]
    fn test_format_cpu_temp() {
        assert_eq!(format_cpu_temp("49000\n"), "49°C");
        assert_eq!(format_cpu_temp("50000"), "50°C");
        assert_eq!(format_cpu_temp("0"), "0°C");
        assert_eq!(format_cpu_temp("-5000"), "-5°C");
        assert_eq!(format_cpu_temp("invalid"), "0°C");
    }

    // --- /proc/stat CPU Usage Tests ---

    #[test]
    fn test_parse_proc_stat_standard() {
        let raw = "\
cpu  82684 2534 39153 1764901 4374 8871 2710 0 100 50
cpu0 42 0 99 118120 18 128 4 0 0 0
cpu1 71 0 89 118089 151 12 2 0 0 0
";
        let sample = parse_proc_stat(raw).expect("valid /proc/stat");
        // idle_all = 1764901 + 4374 = 1769275
        // non_idle = 82684 + 2534 + 39153 + 8871 + 2710 + 0 = 135952
        // guest (100) and guest_nice (50) must NOT be double-counted
        assert_eq!(sample.idle_all, 1_769_275);
        assert_eq!(sample.total, 1_769_275 + 135_952);
    }

    #[test]
    fn test_parse_proc_stat_minimal_four_fields() {
        let raw = "cpu  100 20 30 850\n";
        let sample = parse_proc_stat(raw).expect("4-field /proc/stat");
        assert_eq!(sample.idle_all, 850);
        assert_eq!(sample.total, 1000);
    }

    #[test]
    fn test_parse_proc_stat_invalid_or_missing() {
        assert_eq!(parse_proc_stat(""), None);
        assert_eq!(parse_proc_stat("cpu0 10 20 30 40\n"), None);
        assert_eq!(parse_proc_stat("cpu  10 20 30\n"), None);
        assert_eq!(parse_proc_stat("cpu  10 abc 30 40\n"), None);
        assert_eq!(parse_proc_stat("cpu  10 20 30 40 oops\n"), None);
    }

    #[test]
    fn test_calculate_cpu_usage_percent() {
        let s1 = CpuStatSample {
            total: 1000,
            idle_all: 800,
        };
        let s2 = CpuStatSample {
            total: 2000,
            idle_all: 1550,
        };
        // total_delta = 1000, idle_delta = 750 -> active = 250 -> 25%
        assert_eq!(calculate_cpu_usage_percent(s1, s2), Some(25));

        let full_busy = CpuStatSample {
            total: 1500,
            idle_all: 800,
        };
        assert_eq!(calculate_cpu_usage_percent(s1, full_busy), Some(100));

        let full_idle = CpuStatSample {
            total: 1500,
            idle_all: 1300,
        };
        assert_eq!(calculate_cpu_usage_percent(s1, full_idle), Some(0));

        // Rounding: 2 / 3 -> 66.66% -> 67%
        let round_up = CpuStatSample {
            total: 1003,
            idle_all: 801,
        };
        assert_eq!(calculate_cpu_usage_percent(s1, round_up), Some(67));
    }

    #[test]
    fn test_calculate_cpu_usage_percent_edge_cases() {
        let s1 = CpuStatSample {
            total: 1000,
            idle_all: 800,
        };
        // Zero delta or counter regression
        assert_eq!(calculate_cpu_usage_percent(s1, s1), None);
        let regressed = CpuStatSample {
            total: 900,
            idle_all: 700,
        };
        assert_eq!(calculate_cpu_usage_percent(s1, regressed), None);

        // Kernel iowait quirk where idle_all slightly decreases while total increases
        let iowait_quirk = CpuStatSample {
            total: 1010,
            idle_all: 799,
        };
        assert_eq!(calculate_cpu_usage_percent(s1, iowait_quirk), Some(100));
    }

    // --- Battery Telemetry Tests ---

    #[test]
    fn test_battery_telemetry_energy_and_power_now() {
        let mock = TempMockDir::new("bat_tel_ux3405");
        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();
        fs::write(bat0.join("capacity"), "79\n").unwrap();
        fs::write(bat0.join("status"), "Discharging\n").unwrap();
        fs::write(bat0.join("power_now"), "11813000\n").unwrap();
        fs::write(bat0.join("energy_full"), "70216000\n").unwrap();
        fs::write(bat0.join("energy_full_design"), "75086000\n").unwrap();
        fs::write(bat0.join("cycle_count"), "38\n").unwrap();

        let tel = read_battery_telemetry_in(&mock.path).expect("telemetry present");
        assert_eq!(tel.capacity_percent, Some(79));
        assert_eq!(tel.status.as_deref(), Some("Discharging"));
        let w = tel.power_watts.expect("power_watts");
        assert!((w - 11.813).abs() < 1e-6);
        let h = tel.health_percent.expect("health_percent");
        assert!((h - (70216000.0 / 75086000.0 * 100.0)).abs() < 1e-6);
        assert_eq!(tel.cycle_count, Some(38));
    }

    #[test]
    fn test_battery_telemetry_current_voltage_and_charge_fallback() {
        let mock = TempMockDir::new("bat_tel_fallback");
        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();
        fs::write(bat0.join("capacity"), "50\n").unwrap();
        fs::write(bat0.join("status"), "Charging\n").unwrap();
        fs::write(bat0.join("current_now"), "-2000000\n").unwrap();
        fs::write(bat0.join("voltage_now"), "12000000\n").unwrap();
        fs::write(bat0.join("charge_full"), "4800000\n").unwrap();
        fs::write(bat0.join("charge_full_design"), "5000000\n").unwrap();

        let tel = read_battery_telemetry_in(&mock.path).expect("telemetry present");
        assert_eq!(tel.capacity_percent, Some(50));
        assert_eq!(tel.status.as_deref(), Some("Charging"));
        assert!((tel.power_watts.unwrap() - 24.0).abs() < 1e-6);
        assert!((tel.health_percent.unwrap() - 96.0).abs() < 1e-6);
        assert_eq!(tel.cycle_count, None);
    }

    #[test]
    fn test_battery_telemetry_ignores_device_and_handles_zero_design() {
        let mock = TempMockDir::new("bat_tel_edge");
        let mouse = mock.path.join("hidpp_battery_0");
        fs::create_dir_all(&mouse).unwrap();
        fs::write(mouse.join("type"), "Battery\n").unwrap();
        fs::write(mouse.join("scope"), "Device\n").unwrap();
        fs::write(mouse.join("capacity"), "90\n").unwrap();

        assert_eq!(read_battery_telemetry_in(&mock.path), None);

        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();
        fs::write(bat0.join("energy_full"), "5000\n").unwrap();
        fs::write(bat0.join("energy_full_design"), "0\n").unwrap();
        fs::write(bat0.join("cycle_count"), "-1\n").unwrap();

        let tel = read_battery_telemetry_in(&mock.path).expect("BAT0 present");
        assert_eq!(tel.health_percent, None);
        assert_eq!(tel.cycle_count, None);
    }

    // --- CPU Power Telemetry & Armoury Permission Tests ---

    #[test]
    fn test_parse_no_turbo() {
        assert_eq!(parse_no_turbo("0\n"), Some(true));
        assert_eq!(parse_no_turbo("1\n"), Some(false));
        assert_eq!(parse_no_turbo("2"), None);
        assert_eq!(parse_no_turbo(""), None);
    }

    #[test]
    fn test_parse_and_clamp_perf_pct() {
        assert_eq!(parse_perf_pct("100\n"), Some(100));
        assert_eq!(parse_perf_pct(" 65 \r\n"), Some(65));
        assert_eq!(parse_perf_pct("0"), Some(0));
        assert_eq!(parse_perf_pct("101"), None);
        assert_eq!(parse_perf_pct("-1"), None);
        assert_eq!(parse_perf_pct(""), None);

        assert_eq!(clamp_perf_pct(0), 30);
        assert_eq!(clamp_perf_pct(8), 30);
        assert_eq!(clamp_perf_pct(30), 30);
        assert_eq!(clamp_perf_pct(65), 65);
        assert_eq!(clamp_perf_pct(100), 100);
        assert_eq!(clamp_perf_pct(150), 100);
    }

    #[test]
    fn test_read_cpu_power_telemetry_in() {
        let mock = TempMockDir::new("cpu_tel");
        let epp = mock.path.join("epp");
        let gov = mock.path.join("gov");
        let no_turbo = mock.path.join("no_turbo");
        let max_perf = mock.path.join("max_perf_pct");
        let min_perf = mock.path.join("min_perf_pct");
        fs::write(&epp, "balance_performance\n").unwrap();
        fs::write(&gov, "powersave\n").unwrap();
        fs::write(&no_turbo, "0\n").unwrap();
        fs::write(&max_perf, "80\n").unwrap();
        fs::write(&min_perf, "13\n").unwrap();

        let tel = read_cpu_power_telemetry_in(&epp, &gov, &no_turbo, &max_perf, &min_perf);
        assert_eq!(tel.epp.as_deref(), Some("balance_performance"));
        assert_eq!(tel.governor.as_deref(), Some("powersave"));
        assert_eq!(tel.turbo_enabled, Some(true));
        assert_eq!(tel.max_perf_pct, Some(80));
        assert_eq!(tel.min_perf_pct, Some(13));
        assert!(tel.pstate_writable);

        let missing = read_cpu_power_telemetry_in(
            &mock.path.join("missing_epp"),
            &mock.path.join("missing_gov"),
            &mock.path.join("missing_turbo"),
            &mock.path.join("missing_max_perf"),
            &mock.path.join("missing_min_perf"),
        );
        assert_eq!(missing, CpuPowerTelemetry::default());
        assert!(!missing.pstate_writable);
    }

    #[test]
    fn test_is_armoury_attr_writable_in() {
        use std::os::unix::fs::PermissionsExt;
        let mock = TempMockDir::new("armoury_perms");
        let ro_dir = mock.path.join("charge_mode");
        let rw_dir = mock.path.join("boot_sound");
        fs::create_dir_all(&ro_dir).unwrap();
        fs::create_dir_all(&rw_dir).unwrap();

        let ro_file = ro_dir.join("current_value");
        let rw_file = rw_dir.join("current_value");
        fs::write(&ro_file, "0\n").unwrap();
        fs::write(&rw_file, "1\n").unwrap();

        fs::set_permissions(&ro_file, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&rw_file, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(!is_armoury_attr_writable_in(&mock.path, "charge_mode"));
        assert!(is_armoury_attr_writable_in(&mock.path, "boot_sound"));
        assert!(!is_armoury_attr_writable_in(&mock.path, "nonexistent"));
    }

    #[test]
    fn test_has_discrete_gpu_in() {
        let mock = TempMockDir::new("drm_igpu");
        let card1 = mock.path.join("card1").join("device");
        let conn = mock.path.join("card1-eDP-1");
        fs::create_dir_all(&card1).unwrap();
        fs::create_dir_all(&conn).unwrap();
        fs::write(card1.join("vendor"), "0x8086\n").unwrap();
        assert_eq!(has_discrete_gpu_in(&mock.path), Some(false));

        let card0 = mock.path.join("card0").join("device");
        fs::create_dir_all(&card0).unwrap();
        fs::write(card0.join("vendor"), "0x10de\n").unwrap();
        assert_eq!(has_discrete_gpu_in(&mock.path), Some(true));

        let empty = TempMockDir::new("drm_empty");
        assert_eq!(has_discrete_gpu_in(&empty.path), None);
    }

    #[test]
    fn test_parse_groups_contains() {
        assert!(parse_groups_contains("thoma wheel input video", "input"));
        assert!(!parse_groups_contains("thoma wheel video", "input"));
        assert!(!parse_groups_contains("thoma input-extra", "input"));
        assert!(!parse_groups_contains("", "input"));
    }

    #[test]
    fn test_can_read_any_input_event_in() {
        let mock = TempMockDir::new("input_ev");
        assert!(!can_read_any_input_event_in(&mock.path));

        fs::write(mock.path.join("mice"), "").unwrap();
        assert!(!can_read_any_input_event_in(&mock.path));

        fs::write(mock.path.join("event0"), "").unwrap();
        assert!(can_read_any_input_event_in(&mock.path));
    }

    #[test]
    fn test_package_install_command_from_os_release() {
        assert_eq!(
            package_install_command_from_os_release("ID=fedora\n", "playerctl", "playerctl", "playerctl"),
            "sudo dnf install -y playerctl"
        );
        assert_eq!(
            package_install_command_from_os_release(
                "ID=ubuntu\nID_LIKE=debian\n",
                "playerctl",
                "playerctl",
                "playerctl"
            ),
            "sudo apt install -y playerctl"
        );
        assert_eq!(
            package_install_command_from_os_release("ID=arch\n", "playerctl", "playerctl", "playerctl"),
            "sudo pacman -S playerctl"
        );
    }

    #[test]
    fn test_is_any_display_powered_in_drm_and_backlight() {
        let drm = TempMockDir::new("drm_power");
        let bl = TempMockDir::new("bl_power");

        // Empty dirs -> fail-open (true)
        assert!(is_any_display_powered_in(&drm.path, &bl.path));

        // Internal eDP-1 connected and On
        let edp = drm.path.join("card1-eDP-1");
        fs::create_dir_all(&edp).unwrap();
        fs::write(edp.join("status"), "connected\n").unwrap();
        fs::write(edp.join("enabled"), "enabled\n").unwrap();
        fs::write(edp.join("dpms"), "On\n").unwrap();

        let hdmi = drm.path.join("card1-HDMI-A-1");
        fs::create_dir_all(&hdmi).unwrap();
        fs::write(hdmi.join("status"), "disconnected\n").unwrap();
        fs::write(hdmi.join("dpms"), "Off\n").unwrap();

        assert!(is_any_display_powered_in(&drm.path, &bl.path));

        // DPMS Off on eDP-1 -> false
        fs::write(edp.join("dpms"), "Off\n").unwrap();
        assert!(!is_any_display_powered_in(&drm.path, &bl.path));

        // DPMS On, but backlight bl_power = 4 (powerdown) -> false for internal panel
        fs::write(edp.join("dpms"), "On\n").unwrap();
        let intel_bl = bl.path.join("intel_backlight");
        fs::create_dir_all(&intel_bl).unwrap();
        fs::write(intel_bl.join("bl_power"), "4\n").unwrap();
        assert!(!is_any_display_powered_in(&drm.path, &bl.path));

        // External HDMI connected & On while internal panel bl_power = 4 -> true
        fs::write(hdmi.join("status"), "connected\n").unwrap();
        fs::write(hdmi.join("enabled"), "enabled\n").unwrap();
        fs::write(hdmi.join("dpms"), "On\n").unwrap();
        assert!(is_any_display_powered_in(&drm.path, &bl.path));

        // Both off -> false
        fs::write(hdmi.join("dpms"), "Off\n").unwrap();
        assert!(!is_any_display_powered_in(&drm.path, &bl.path));

        // Backlight unblanked (0) -> true
        fs::write(intel_bl.join("bl_power"), "0\n").unwrap();
        assert!(is_any_display_powered_in(&drm.path, &bl.path));
    }

    #[test]
    fn test_is_lid_closed_in() {
        let lid = TempMockDir::new("acpi_lid");
        assert!(!is_lid_closed_in(&lid.path));

        let lid0 = lid.path.join("LID0");
        fs::create_dir_all(&lid0).unwrap();
        fs::write(lid0.join("state"), "state:      open\n").unwrap();
        assert!(!is_lid_closed_in(&lid.path));

        fs::write(lid0.join("state"), "state:      closed\n").unwrap();
        assert!(is_lid_closed_in(&lid.path));
    }
}
