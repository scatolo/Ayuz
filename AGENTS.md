# AGENTS.md

Repository guidance for coding agents and contributors working on **Ayuz** (`scatolo/Ayuz`), a GTK4 / Libadwaita / Relm4 control center for ASUS laptops on Linux.

---

## 1. Fork Identity & Target Scope

- **Fork**: `scatolo/Ayuz` (downstream fork of `Traciges/Ayuz`), package version `1.1.10-ux3405.1` (`Cargo.toml`), Rust `edition = "2024"`, license `GPL-3.0-or-later`.
- **Primary Fork Target**: **ASUS Zenbook 14 OLED (UX3405CA)** running **Fedora 44 · GNOME (Wayland)**.
- **Upstream Target Retained**: **ASUS Zenbook S16** running **Fedora 43 · KDE Plasma (Wayland)** (and COSMIC where supported).
- **Explicit Scope Boundaries**:
  - **Display Color Gamut switching** (`src/components/display/color_gamut.rs`) is KDE-only (`is_kde_desktop()` / `kscreen-doctor`) and is **explicitly out of scope** for GNOME / UX3405CA. Do not add GNOME/Mutter ICC/gamut switching unless explicitly requested.
  - **Touchpad NumberPad**: the fork maintainer's UX3405CA SKU has **no physical or digital NumberPad LED grid** on the touchpad (even though `ASUP*` touchpads expose an I2C bus). Keep `UX3405MA_5X4` and DMI substring `"UX3405"` intact in `src/services/numberpad_layouts.rs` for UX3405 SKUs that *do* have hardware NumberPad LEDs, and keep docs/warnings accurate for touchpad-only SKUs (only the `input` group is required for Smart Gestures and While Typing).

Supporting hardware/OS details and the UX3405CA compatibility matrix live in [docs/UX3405CA_SUPPORT.md](docs/UX3405CA_SUPPORT.md).

---

## 2. Build, Test, Lint, and Packaging Commands

### System Build Dependencies

- **Fedora 44** (used by CI `check` and `build-rpm`):
  ```bash
  sudo dnf install -y \
    gcc pkgconf-pkg-config \
    gtk4-devel libadwaita-devel gtk4-layer-shell-devel dbus-devel \
    binutils
  ```
  (`binutils` provides `strip` for RPM builds.)
- **Ubuntu / Debian** (used by CI `build-deb` and `build-appimage` on `ubuntu:26.04`):
  ```bash
  sudo apt-get install -y \
    build-essential pkg-config \
    libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev libdbus-1-dev \
    dpkg-dev file desktop-file-utils
  ```

### Everyday Cargo Commands

```bash
# Run headless unit tests (must pass without a Wayland/X11 display, D-Bus, or ASUS hardware)
cargo test --locked

# Lint all targets (CI runs clippy on Fedora 44)
cargo clippy --all-targets --locked

# Build release binary
cargo build --release --locked

# Run locally (CLI flags accepted by src/main.rs)
cargo run --locked
cargo run --locked -- --hidden
cargo run --locked -- --toggle-numberpad
```

### Packaging & Release Artifacts

CI workflow `.github/workflows/release.yml` runs on pushes to `main`, tags matching `v*`, or manual `workflow_dispatch` (optional `force_release` boolean):

- **`prepare` & `check`** (run on every trigger; `check` runs in `quay.io/fedora/fedora:44`): `cargo test --locked` and `cargo clippy --all-targets --locked`.
- **`build-rpm`**, **`build-deb`**, **`build-appimage`**, and **`release`** only run when `check` passes and `should_release == 'true'` (new tag, new version on `main`, or `workflow_dispatch` with missing tag/release or `force_release=true`):
  - **RPM** (`quay.io/fedora/fedora:44`, `cargo-generate-rpm`):
    ```bash
    PKG_VERSION="1.1.10-ux3405.1"      # must match Cargo.toml [package].version
    RPM_VERSION="${PKG_VERSION//-/+}"  # e.g. 1.1.10+ux3405.1
    cargo build --release --locked && strip -s target/release/ayuz
    cargo generate-rpm -s "version = \"${RPM_VERSION}\""
    ```
  - **DEB** (`ubuntu:26.04`, `cargo-deb`):
    ```bash
    PKG_VERSION="1.1.10-ux3405.1"
    DEB_VERSION="${PKG_VERSION//-/+}-1" # e.g. 1.1.10+ux3405.1-1
    cargo build --release --locked
    cargo deb --no-build --deb-version "${DEB_VERSION}"
    ```
  - **AppImage** (`ubuntu:26.04`, `cargo-appimage` + `appimagetool` on `$PATH`):
    ```bash
    PKG_VERSION="1.1.10-ux3405.1"
    cargo build --release --locked
    VERSION="${PKG_VERSION}" ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 cargo appimage
    ```

**Version bump rule**: Because CI runs all Cargo steps with `--locked`, whenever you bump the version keep all three synchronized:
1. `Cargo.toml` `[package].version` (e.g. `1.1.10-ux3405.1`)
2. `Cargo.toml` `[package.metadata.generate-rpm].version` (`-` replaced with `+`, e.g. `1.1.10+ux3405.1`)
3. `Cargo.lock` root `ayuz` package version

Note: `Cargo.toml` `[package].repository`, the sidebar GitHub button in `src/app.rs`, `flake.nix`, and `packaging/de.guido.ayuz.metainfo.xml` still retain upstream `Traciges/Ayuz` metadata/release history.

---

## 3. Codebase Map

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | Tracing init, config & locale loading, `--toggle-numberpad` / `--hidden` CLI arg stripping before GTK4, `adw::init()`, CSS (`assets/style.css`) & `RelmApp` startup (`de.guido.ayuz`). |
| `src/app.rs` | `AppModel` (`adw::NavigationSplitView` + `adw::ViewStack`), `distribute_profile()`, `widget_map` for search scroll-to-widget, abstract Unix socket listener (`\0ayuz-numberpad`), `fan_hotkey` task spawn, ASUS key hint / language & autostart / migration groups. |
| `src/sys_paths.rs` | Sysfs/proc constants, `anime_assets_dir()`, dynamic `/sys/class/power_supply/*` battery discovery (`find_battery_capacity_paths*`), dynamic `/sys/class/hwmon` CPU temp lookup (`coretemp`, `k10temp`, `zenpower`, `*cpu*`) with `thermal_zone0` fallback. |
| `src/search.rs` | Sidebar `NAV_ITEMS` / `sorted_nav_items()` and `SEARCH_INDEX` + animated scroll-to-widget (`scroll_to_widget`). |
| `src/tray.rs` | `ksni::Tray` system tray icon (`assets/trayicon.png`) and Show/Exit menu. |
| `src/autostart.rs` | Manages `~/.config/autostart/de.guido.ayuz.desktop` with `Exec=ayuz --hidden`. |
| `src/services/config.rs` | `AppConfig` & `Profile` JSON persistence at `~/.config/ayuz/config.json` (`ProjectDirs::from("", "", "ayuz")`). |
| `src/services/migration.rs` | Legacy `~/.config/asus-hub/config.json` detection and migration prompts. |
| `src/services/commands.rs` | `run_command_blocking`, `&'static str`-guarded `pkexec_read_file` / `pkexec_write_sysfs`, `which_exists`, `resolve_qdbus_path`, `is_kde_desktop` / `is_gnome_desktop` / `is_cosmic_desktop`. |
| `src/services/dbus.rs` | `zbus` system-bus proxies for `xyz.ljones.Asusd` (`Platform`, `FanProfile`, charge limit, `AsusArmoury` `apu_mem`, `Aura` discovery/control) and `org.supergfxctl.Daemon` (`GfxMode`). |
| `src/services/dbus_animatrix.rs` | DMI AniMatrix board detection (`detect_animatrix_hardware`) and `zbus` proxy (`xyz.ljones.Anime`) for AniMatrix controls and GIF frame streaming. |
| `src/services/evdev_runner.rs` | Shared `/dev/input/` helpers (`find_touchpad` skipping `ayuz*` virtual devices, `find_keyboards`, `touchpad_abs_bounds`, `open_event_stream`). |
| `src/services/edge_gestures.rs` | Touchpad edge-swipe loop: left edge = volume (`wpctl` preferred, `pactl` fallback), right edge = brightness (KDE D-Bus / GNOME D-Bus / `brightnessctl`), top edge = media (`playerctl`). |
| `src/services/gnome_brightness.rs` | Session D-Bus brightness via `org.gnome.SettingsDaemon.Power.Screen` with fallback to `org.gnome.Mutter.DisplayConfig` (`Backlight` / `SetBacklight` for GNOME 47+ Wayland). |
| `src/services/kde_brightness.rs` | KDE PowerDevil D-Bus brightness control (`org.kde.Solid.PowerManagement.Actions.BrightnessControl`). |
| `src/services/touchpad_ctl.rs` | DE-agnostic touchpad enable/disable (`gsettings` on GNOME, `qdbus` on KDE). |
| `src/services/typing_watch.rs` | Keyboard monitoring (`KEY_ESC`..`KEY_COMPOSE` excluding modifiers) for palm rejection (suppresses edge gestures and/or disables touchpad while typing). |
| `src/services/numberpad.rs`, `numberpad_layouts.rs`, `numberpad_pointer.rs` | In-process ASUS NumberPad I2C (`0x15` / `0x38`) LED packet + `uinput` keycode emitter + `PointerRelay`. `for_product()` maps `UX3405`, `UM3402`, `B3302` to `UX3405MA_5X4` and defaults to `UNIVERSAL_4X4`. |
| `src/services/fan_hotkey.rs` | Listens for ASUS WMI fan keycodes (`204`, `425`, `467`, `482`) and emits `FanMsg::CycleFromHotkey`. |
| `src/components/widgets.rs`, `home.rs` | Shared `DaemonWarningLabel` template and `HomeModel` (DMI/kernel info, serial reveal via `pkexec`, battery/CPU/RAM/disk metrics, profile cards). |
| `src/components/display/` | `oled_dimming.rs` (KDE `kscreen-doctor`), `target_mode.rs` (KDE KWin `diminactive`), `oled_care.rs` (KDE `kwriteconfig6` pixel refresh + KDE/COSMIC panel autohide/transparency), `cosmic_panel.rs`, `color_gamut.rs` (KDE-only), `helpers.rs`. |
| `src/components/keyboard/` | `auto_backlight.rs` (`iio-sensor-proxy` + `logind` lock check), `backlight_idle.rs` (`swayidle` + UPower D-Bus), `fn_key.rs` (`pkexec grubby` `asus_wmi.fnlock_default`). |
| `src/components/aura/`, `animatrix/` | Dynamic `asusd` Aura RGB device pages (`aura_page.rs`, `aura_device.rs`) and AniMatrix LED matrix controls + GIF gallery (`animatrix.rs`). |
| `src/components/touchpad/` | `touchpad.rs` (10s auto-revert safety timer), `numberpad.rs`, `gestures.rs` (requires `playerctl`; brightness gate satisfied on GNOME/KDE or `brightnessctl`), `typing.rs`. |
| `src/components/audio/`, `system/` | `volume.rs` (`wpctl` + optional `pactl subscribe` listener), `sound_modes.rs` (`easyeffects`), `battery.rs` (`asusd` charge limit + `/sys/power/mem_sleep`), `fan.rs` + `fan_osd.rs` (`gtk4-layer-shell` with regular window fallback), `gpu.rs` (`supergfxctl`), `apu_mem.rs` (`asusd`). |
| `locales/` | `en.yml`, `de.yml`, `pt-br.yml` (`rust-i18n`, fallback `"en"`). |
| `packaging/`, `.github/workflows/` | `.desktop` files, AppStream metainfo (`de.guido.ayuz.metainfo.xml`), `udev/99-ayuz-numberpad.rules`, `release.yml`, plus root `dist-workspace.toml` and `flake.nix`. |
| `assets/` | `style.css`, `trayicon.png`, `img/`, `icm/`, `presets/` (EasyEffects), `anime/` (AniMatrix GIFs/images). |

---

## 4. Coding Conventions & Critical Guardrails

1. **License header**: Every `.rs` file starts with the standard GPL-3.0-or-later header (`Copyright (C) 2026 Guido Philipp`).
2. **Relm4 `#[watch]` feedback-loop guard**: Relm4 re-evaluates `#[watch]` bindings on every `update`/`update_cmd`. Always start toggle/selection handlers with an early return (`if new_val == self.field { return; }`) before mutating state, writing config, or spawning commands.
3. **Never block the GTK main thread**: Offload subprocesses and blocking I/O via `commands::run_command_blocking`, `tokio::spawn`, or `sender.command(...)`.
4. **CLI flags**: GTK4 aborts on unrecognized flags before application code runs. Any new CLI flag must be parsed and stripped in `src/main.rs` before `RelmApp::with_args(gtk_args)`.
5. **Polkit / `pkexec` safety**: `pkexec_read_file` and `pkexec_write_sysfs` in `src/services/commands.rs` take `&'static str` only to prevent path/command injection. Keep those signatures restricted to static literals.
6. **Graceful feature gating**: Missing tools, missing kernel permissions (`/dev/input`, `/dev/i2c-*`, `/dev/uinput`), unavailable daemons (`asusd`, `supergfxd`, `iio-sensor-proxy`), or non-matching desktop environments must never panic. Gate unsupported controls via `set_sensitive` + `DaemonWarningLabel` (or surface subprocess errors via `sender.output(...)` toast notifications, as in `backlight_idle.rs`).
7. **Desktop detection**: Use `commands::is_kde_desktop()`, `is_gnome_desktop()`, and `is_cosmic_desktop()` (`XDG_CURRENT_DESKTOP` case-insensitive substring check). Keep GNOME display features disabled unless explicitly requested; never expand `ColorGamutModel` to GNOME.
8. **Checklist for adding a new profile setting**:
   - Add field to `Profile` in `src/services/config.rs` with `#[serde(default = "...")]` and update `Profile::default()`.
   - Emit `LoadProfile` in `AppModel::distribute_profile` (`src/app.rs`).
   - Add widget to `widget_map` in `src/app.rs` and to `SEARCH_INDEX` in `src/search.rs`.
   - Add translation keys to **all three** files: `locales/en.yml`, `locales/de.yml`, and `locales/pt-br.yml`.
9. **Headless unit tests**: CI runs `cargo test --locked` inside `quay.io/fedora/fedora:44` with no display server, no D-Bus session/system services, and no ASUS hardware. Keep unit tests decoupled from live `/sys`, `/dev`, D-Bus, and GTK display initialization (follow the mock-directory pattern in `src/sys_paths.rs` and pure-function tests in `src/services/gnome_brightness.rs`, `src/services/edge_gestures.rs`, and `src/services/numberpad_layouts.rs`).
