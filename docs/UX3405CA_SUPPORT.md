# ASUS Zenbook 14 OLED (UX3405CA) & Fedora 44 GNOME Wayland Support

Technical reference for the `scatolo/Ayuz` fork (`1.1.10-ux3405.1`), documenting how Ayuz adapts to **ASUS Zenbook 14 OLED (UX3405CA)** on **Fedora 44 · GNOME (Wayland)** and how automated releases are built.

---

## 1. Feature & Compatibility Matrix (UX3405CA · Fedora 44 GNOME Wayland)

| Page | Feature | Status on UX3405CA / Fedora 44 GNOME | Backend / Notes |
| --- | --- | --- | --- |
| **Home** | Device info & telemetry | Supported | Dynamic sysfs discovery in `src/sys_paths.rs` |
| **Home** | Profiles | Supported | Persisted in `~/.config/ayuz/config.json` |
| **Display** | OLED Flicker-Free Dimming | Disabled (KDE only) | Requires `kscreen-doctor` / KDE PowerDevil |
| **Display** | Color Gamut | **Excluded from scope** (KDE only) | Requires `kscreen-doctor`; intentionally not ported to GNOME |
| **Display** | Target Mode | Disabled (KDE only) | Requires KWin `diminactive` + `qdbus` / `kwriteconfig6` |
| **Display** | OLED Pixel Refresh / Panel settings | Disabled (KDE / COSMIC only) | Uses KDE `kwriteconfig6` / `qdbus` or COSMIC panel helpers |
| **Keyboard** | Auto Backlight | Optional | Requires `iio-sensor-proxy` |
| **Keyboard** | Backlight Idle | Optional | Requires `swayidle` |
| **Keyboard** | FN Key Mode & FN+F Hotkey | Optional | Requires `asusd`; `fan_hotkey.rs` listens for keycodes `204`, `425`, `467`, `482` |
| **Aura / AniMatrix** | RGB / LED Matrix | Hardware-dependent / Grayed out | Zenbook UX3405CA does not have ROG AniMatrix hardware |
| **Touchpad** | Touchpad Toggle | Supported | `gsettings set org.gnome.desktop.peripherals.touchpad send-events` (`touchpad_ctl.rs`) |
| **Touchpad** | Smart Gestures | Supported | Requires `input` group; uses `wpctl`, GNOME D-Bus brightness, `playerctl` |
| **Touchpad** | While Typing | Supported | Requires `input` group; uses `typing_watch.rs` + `touchpad_ctl.rs` |
| **Touchpad** | NumberPad | **SKU-dependent** | Maintainer's UX3405CA SKU has no physical/digital NumberPad LEDs; `UX3405` DMI override (`UX3405MA_5X4` 5×4) applies if LED hardware is present |
| **Audio** | Volume & Boost (0–150%) | Supported | `wpctl` (`pipewire-utils`) |
| **Audio** | Sound Profiles | Optional | `easyeffects` + bundled presets in `assets/presets/` |
| **System** | Battery Care & Fan Profiles | Optional | `asusd` (`xyz.ljones.Asusd`) |
| **System** | Deep Sleep (`s2idle` / `deep`) | Supported | Reads/writes `/sys/power/mem_sleep` via `pkexec` |
| **System** | GPU Mode / APU Mem | Optional / Hardware-dependent | `supergfxd` / `asusd` |

---

## 2. UX3405CA Port Implementation Details

### 2.1 Dynamic Sysfs Telemetry (`src/sys_paths.rs`, `src/components/home.rs`)

- **Battery discovery**: `find_battery_capacity_paths()` scans `/sys/class/power_supply/*` for entries whose `type` file equals `Battery` (case-insensitive) and includes a `capacity` file, ignoring `Mains` and `USB` supplies and averaging across discovered batteries (`aggregate_battery_percentage()`).
- **CPU temperature discovery**: `find_cpu_temp_path()` scans `/sys/class/hwmon/*` (sorted) for `name` matching `coretemp` (Intel Core Ultra on UX3405CA), `k10temp`, `zenpower`, or containing `cpu`. It prefers `temp1_input`, falls back to the first `temp*_input`, and finally falls back to `/sys/class/thermal/thermal_zone0/temp`.

### 2.2 Smart Gestures on GNOME Wayland (`src/services/edge_gestures.rs`, `src/services/gnome_brightness.rs`)

- **Left edge (Volume)**: `adjust_volume()` checks `/usr/bin/wpctl` (or `wpctl` on `$PATH`) via `OnceLock` and uses PipeWire (`wpctl set-mute` / `set-volume @DEFAULT_AUDIO_SINK@ 5%+/-`), falling back to `pactl`.
- **Right edge (Screen Brightness)**: When `is_gnome_desktop()` is true, `gnome_brightness::adjust_brightness_relative(delta)` runs on the session D-Bus:
  1. Tries `org.gnome.SettingsDaemon.Power.Screen` (`StepUp` / `StepDown`, then `Brightness` property).
  2. Falls back to `org.gnome.Mutter.DisplayConfig` (`Backlight` property + `SetBacklight(serial, connector, next)`), which handles GNOME 47+ Wayland (including Fedora 44). `calculate_next_backlight()` computes a proportional step across the hardware span (e.g., `4..496` on UX3405CA) with a minimum 1-unit change when `delta_percent != 0`.
  3. Falls back to `brightnessctl set 5%+/-` if D-Bus calls fail.
- **Top edge (Media)**: Calls `playerctl previous` / `playerctl next`.
- **UI gating (`src/components/touchpad/gestures.rs`)**: `brightnessctl_available` is treated as satisfied when `is_gnome_desktop() || is_kde_desktop() || which_exists("brightnessctl")`, so GNOME Wayland users do not need `brightnessctl` installed to enable Smart Gestures.

### 2.3 NumberPad & Touchpad-Only UX3405CA SKUs (`src/services/numberpad.rs`, `src/services/numberpad_layouts.rs`)

- `numberpad_layouts::for_product()` matches DMI `product_name` substrings (`"UX3405"`, `"UM3402"`, `"B3302"`) to `UX3405MA_5X4` (5 columns × 4 rows, stacked `Backspace` in rows 0–1 col 4, `Shift+5` macro for `%`), defaulting all other laptops to `UNIVERSAL_4X4`.
- Some UX3405CA SKUs have a standard touchpad **without** physical or digital NumberPad LED hardware, even though the `ASUP*` touchpad controller appears on an I2C bus in `/proc/bus/input/devices`.
  - On touchpad-only SKUs, users only need `sudo usermod -aG input $USER` for **Smart Gestures** and **While Typing**.
  - Neither `i2c-dev` nor the `i2c` group is needed unless the touchpad physically includes the LED NumberPad grid; any `/dev/i2c-*` warning on the NumberPad row is harmless and does not affect gestures or palm rejection.

---

## 3. Automated CI & Packaging Pipeline (`.github/workflows/release.yml`)

Every push to `main`, push of a `v*` tag, or manual `workflow_dispatch` runs:

1. **`prepare`**: Reads `package.version` from `Cargo.toml` (or strips `v` from tag ref), sets `tag=v<version>`, and determines `should_release`:
   - On tag push: always releases.
   - On `main` push: releases if tag `v<version>` does not yet exist on `origin`.
   - On `workflow_dispatch`: releases if `force_release == true`, tag is missing, or GitHub Release is missing.
2. **`check`** (`quay.io/fedora/fedora:44`): Installs GTK4/Libadwaita/LayerShell/D-Bus devel packages + Rust stable (`clippy`) and runs:
   - `cargo test --locked`
   - `cargo clippy --all-targets --locked`
3. **Package builds** (gated on `should_release == 'true'` and `check`):
   - `build-rpm` (`quay.io/fedora/fedora:44`): `cargo build --release --locked`, `strip -s target/release/ayuz`, `cargo generate-rpm -s "version = \"${PKG_VERSION//-/+}\""`
   - `build-deb` (`ubuntu:26.04`): `cargo build --release --locked`, `cargo deb --no-build --deb-version "${PKG_VERSION//-/+}-1"`
   - `build-appimage` (`ubuntu:26.04`): `cargo build --release --locked`, `VERSION="${PKG_VERSION}" ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 cargo appimage`
4. **`release`**: Downloads all three artifacts and creates/updates the GitHub Release and tag via `softprops/action-gh-release@v2`.
