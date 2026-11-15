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

use std::path::Path;

use crate::sys_paths::SYS_POWER_SUPPLY_DIR;

#[derive(Debug, Clone, Copy)]
pub(super) struct AmbientRules {
    pub auto_brighten: bool,
    pub brighten_threshold: f64,
    pub auto_dim: bool,
    pub dim_threshold: f64,
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.KbdBacklight",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/KbdBacklight"
)]
pub(super) trait KbdBacklight {
    #[zbus(name = "GetMaxBrightness")]
    fn get_max_brightness(&self) -> zbus::Result<i32>;

    #[zbus(name = "GetBrightness")]
    fn get_brightness(&self) -> zbus::Result<i32>;

    #[zbus(name = "SetBrightness")]
    fn set_brightness(&self, value: i32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "net.hadess.SensorProxy",
    default_service = "net.hadess.SensorProxy",
    default_path = "/net/hadess/SensorProxy"
)]
pub(super) trait SensorProxy {
    fn claim_light(&self) -> zbus::Result<()>;
    fn release_light(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn light_level(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn has_ambient_light(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower"
)]
pub(super) trait UPower {
    #[zbus(property, name = "OnBattery")]
    fn on_battery(&self) -> zbus::Result<bool>;
}

/// Queries the maximum keyboard backlight level from `org.freedesktop.UPower.KbdBacklight`.
///
/// Falls back to `3` if UPower is unreachable or reports a non-positive value.
pub(super) async fn get_kbd_max_brightness() -> i32 {
    let Ok(conn) = zbus::Connection::system().await else {
        return 3;
    };
    let Ok(proxy) = KbdBacklightProxy::new(&conn).await else {
        return 3;
    };
    proxy
        .get_max_brightness()
        .await
        .ok()
        .filter(|&v| v > 0)
        .unwrap_or(3)
}

/// Returns `true` if the system is currently running on battery power.
///
/// Prefers `org.freedesktop.UPower` (`OnBattery`) over `sys_conn`, falling back to
/// `/sys/class/power_supply/*/online` if UPower cannot be queried.
pub(super) async fn is_on_battery(sys_conn: &zbus::Connection) -> bool {
    if let Ok(proxy) = UPowerProxy::builder(sys_conn)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        && let Ok(on_bat) = proxy.on_battery().await
    {
        return on_bat;
    }

    is_on_battery_sysfs_in(Path::new(SYS_POWER_SUPPLY_DIR))
}

/// Reads `/sys/class/power_supply/*/online` under `base_dir` to determine whether the
/// machine is on battery power (`true` if at least one mains/USB supply exists and none
/// report `"1"`).
pub(super) fn is_on_battery_sysfs_in(base_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return false;
    };

    let mut found_online_file = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if std::fs::read_to_string(path.join("type"))
            .is_ok_and(|t| t.trim().eq_ignore_ascii_case("battery"))
        {
            continue;
        }
        if let Ok(online) = std::fs::read_to_string(path.join("online")) {
            found_online_file = true;
            if online.trim() == "1" {
                return false;
            }
        }
    }

    found_online_file
}

/// Reads the current ambient light level (lux) from `net.hadess.SensorProxy` without caching.
pub(super) async fn read_ambient_lux(sys_conn: &zbus::Connection) -> Option<f64> {
    let proxy = SensorProxyProxy::builder(sys_conn)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .ok()?;
    if !proxy.has_ambient_light().await.unwrap_or(false) {
        return None;
    }
    proxy.light_level().await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempMockDir {
        path: PathBuf,
    }

    impl TempMockDir {
        fn new(prefix: &str) -> Self {
            let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
            let unique_name = format!("ayuz_kbd_test_{}_{}_{}", prefix, std::process::id(), count);
            let path = std::env::temp_dir().join(unique_name);
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempMockDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_is_on_battery_sysfs_ac_online() {
        let mock = TempMockDir::new("ac_online");
        let ac0 = mock.path.join("AC0");
        fs::create_dir_all(&ac0).unwrap();
        fs::write(ac0.join("type"), "Mains\n").unwrap();
        fs::write(ac0.join("online"), "0\n").unwrap();

        let usbc = mock.path.join("ucsi-source-psy-USBC000:001");
        fs::create_dir_all(&usbc).unwrap();
        fs::write(usbc.join("type"), "USB\n").unwrap();
        fs::write(usbc.join("online"), "1\n").unwrap();

        assert!(!is_on_battery_sysfs_in(&mock.path));
    }

    #[test]
    fn test_is_on_battery_sysfs_discharging() {
        let mock = TempMockDir::new("discharging");
        let ac0 = mock.path.join("AC0");
        fs::create_dir_all(&ac0).unwrap();
        fs::write(ac0.join("type"), "Mains\n").unwrap();
        fs::write(ac0.join("online"), "0\n").unwrap();

        let bat0 = mock.path.join("BAT0");
        fs::create_dir_all(&bat0).unwrap();
        fs::write(bat0.join("type"), "Battery\n").unwrap();

        assert!(is_on_battery_sysfs_in(&mock.path));
    }

    #[test]
    fn test_is_on_battery_sysfs_empty_dir() {
        let mock = TempMockDir::new("empty");
        assert!(!is_on_battery_sysfs_in(&mock.path));
    }
}
