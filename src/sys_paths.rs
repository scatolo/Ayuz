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
pub const SYS_BATTERY0_CAPACITY: &str = "/sys/class/power_supply/BAT0/capacity";
pub const SYS_BATTERY1_CAPACITY: &str = "/sys/class/power_supply/BAT1/capacity";
pub const SYS_LOAD_AVG: &str = "/proc/loadavg";
pub const SYS_MEM_INFO: &str = "/proc/meminfo";
pub const SYS_HWMON_DIR: &str = "/sys/class/hwmon";
pub const SYS_THERMAL_ZONE0_TEMP: &str = "/sys/class/thermal/thermal_zone0/temp";
pub const SYS_MEM_SLEEP: &str = "/sys/power/mem_sleep";
pub const DEV_UINPUT: &str = "/dev/uinput";
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

/// Dynamically locates all battery capacity sysfs paths under `SYS_POWER_SUPPLY_DIR`.
pub fn find_battery_capacity_paths() -> Vec<PathBuf> {
    find_battery_capacity_paths_in(Path::new(SYS_POWER_SUPPLY_DIR))
}

/// Dynamically locates all battery capacity sysfs paths under `base_dir`.
/// Scans entries for a `type` file containing "Battery" (case-insensitive)
/// and returns the path to `capacity` if present.
pub fn find_battery_capacity_paths_in(base_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return Vec::new();
    };

    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    dirs.sort();

    let mut paths = Vec::new();
    for dir in dirs {
        let type_path = dir.join("type");
        if let Ok(content) = std::fs::read_to_string(&type_path) {
            if content.trim().eq_ignore_ascii_case("battery") {
                let cap_path = dir.join("capacity");
                if cap_path.exists() {
                    paths.push(cap_path);
                }
            }
        }
    }

    paths
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

/// Aggregates a slice of battery capacity percentages (0-100) into an average percentage string.
/// Returns "N/A" if the slice is empty.
pub fn aggregate_battery_percentage(capacities: &[u32]) -> String {
    if capacities.is_empty() {
        "N/A".to_string()
    } else {
        let avg = capacities.iter().sum::<u32>() / capacities.len() as u32;
        format!("{}%", avg)
    }
}

/// Formats a raw millidegree Celsius string into a whole degrees string with unit ("<deg>°C").
pub fn format_cpu_temp(raw_millideg: &str) -> String {
    let millideg: i32 = raw_millideg.trim().parse().unwrap_or(0);
    format!("{}°C", millideg / 1000)
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
    fn test_aggregate_battery_percentage() {
        assert_eq!(aggregate_battery_percentage(&[]), "N/A");
        assert_eq!(aggregate_battery_percentage(&[85]), "85%");
        assert_eq!(aggregate_battery_percentage(&[80, 60]), "70%");
        assert_eq!(aggregate_battery_percentage(&[0]), "0%");
    }

    #[test]
    fn test_format_cpu_temp() {
        assert_eq!(format_cpu_temp("49000\n"), "49°C");
        assert_eq!(format_cpu_temp("50000"), "50°C");
        assert_eq!(format_cpu_temp("0"), "0°C");
        assert_eq!(format_cpu_temp("-5000"), "-5°C");
        assert_eq!(format_cpu_temp("invalid"), "0°C");
    }
}
