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

Supporting hardware/OS and release notes live in [docs/UX3405CA_SUPPORT.md](docs/UX3405CA_SUPPORT.md).

---

## 2. Build, Test, Lint, and Packaging Commands

### System Build Dependencies (Fedora 44)

```bash
sudo dnf install -y \
  gcc pkgconf-pkg-config \
  gtk4-devel libadwaita-devel gtk4-layer-shell-devel dbus-devel \
  binutils
```

(Ubuntu/Debian equivalent used in CI: `build-essential pkg-config libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev libdbus-1-dev`.)

### Everyday Cargo Commands

```bash
# Run headless unit tests (must pass without a Wayland/X11 display, D-Bus, or ASUS hardware)
cargo test --locked

# Lint all targets (CI runs clippy on Fedora 44)
cargo clippy --all-targets --locked

# Build release binary
cargo build --release --locked

# Run locally (CLI flags accepted by main.rs)
cargo run --locked
cargo run --locked -- --hidden
cargo run --locked -- --toggle-numberpad
```

### Packaging & Release Artifacts

CI workflow `.github/workflows/release.yml` runs on pushes to `main`, tags matching `v*`, or `workflow_dispatch`:

- **Check job** (`quay.io/fedora/fedora:44`): `cargo test --locked` and `cargo clippy --all-targets --locked`.
- **RPM** (`quay.io/fedora/fedora:44`, `cargo-generate-rpm`):
  ```bash
  cargo build --release --locked && strip -s target/release/ayuz
  RPM_VERSION="${PKG_VERSION//-/+}"   # e.g. 1.1.10-ux3405.1 -> 1.1.10+ux3405.1
  cargo generate-rpm -s "version = \"${RPM_VERSION}\""
  ```
- **DEB** (`ubuntu:26.04`, `cargo-deb`):
  ```bash
  cargo build --release --locked
  DEB_VERSION="${PKG_VERSION//-/+}-1" # e.g. 1.1.10+ux3405.1-1
  cargo deb --no-build --deb-version "${DEB_VERSION}"
  ```
- **AppImage** (`ubuntu:26.04`, `cargo-appimage` + `appimagetool`):
  ```bash
  cargo build --release --locked
  VERSION="${PKG_VERSION}" ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 cargo appimage
  ```

**Version bump rule**: When bumping the version, keep all three synchronized:
1. `Cargo.toml` `[package].version` (e.g. `1.1.10-ux3405.1`)
2. `Cargo.toml` `[package.metadata.generate-rpm].version` (`-` replaced with `+`, e.g. `1.1.10+ux3405.1`)
3. `Cargo.lock` root `ayuz` package version

---

## 3. Codebase Map

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | Tracing init, config & locale loading, `--toggle-numberpad` / `--hidden` CLI arg stripping before GTK4, `adw::init()`, CSS & `RelmApp` startup (`de.guido.ayuz`). |
| `src/app.rs` | `AppModel` (`adw::NavigationSplitView` + `adw::ViewStack`), `distribute_profile()`, `widget_map` for search scroll-to-widget, abstract Unix socket listener (`\0ayuz-numberpad`), `fan_hotkey` task spawn, migration dialogs. |
| `src/sys_paths.rs` | Sysfs/proc constants, `anime_assets_dir()`, dynamic `/sys/class/power_supply/*` battery discovery, dynamic `/sys/class/hwmon` CPU temp lookup (`coretemp`, `k10temp`, `zenpower`, `*cpu*`) with `thermal_zone0` fallback. |
| `src/search.rs` | Sidebar `NAV_ITEMS` / `sorted_nav_items()` and `SEARCH_INDEX` + animated scroll-to-widget (`scroll_to_widget`). |
| `src/tray.rs` | `ksni::Tray` system tray icon (`assets/trayicon.png`) and Show/Exit menu. |
| `src/autostart.rs` | Manages `~/.config/autostart/de.guido.ayuz.desktop` with `Exec=ayuz --hidden`. |
| `src/services/config.rs` | `AppConfig` & `Profile` JSON persistence at `~/.config/ayuz/config.json` (`ProjectDirs::from("", "", "ayuz")`). |
| `src/services/migration.rs` | Legacy config detection and migration prompts. |
| `src/services/commands.rs` | `run_command_blocking`, `&'static str`-guarded `pkexec_read_file` / `pkexec_write_sysfs`, `which_exists`, `resolve_qdbus_path`, `is_kde_desktop` / `is_gnome_desktop` / `is_cosmic_desktop`. |
| `src/services/dbus.rs` | `zbus` system-bus proxies for `xyz.ljones.Asusd` (`Platform`, `FanProfile`, charge limit) and `org.supergfxctl.Daemon` (`GfxMode`), plus `Aura` constants. |
| `src/services/dbus_animatrix.rs` | `zbus` proxy for AniMatrix (`asusd`) controls and GIF streaming. |
| `src/services/evdev_runner.rs` | Shared `/dev/input/` helpers (`find_touchpad` skipping `ayuz*` virtual devices, `find_keyboards`, `touchpad_abs_bounds`). |
| `src/services/edge_gestures.rs` | Touchpad edge-swipe loop: left edge = volume (`wpctl` preferred, `pactl` fallback), right edge = brightness (KDE D-Bus / GNOME D-Bus / `brightnessctl`), top edge = media (`playerctl`). |
| `src/services/gnome_brightness.rs` | Session D-Bus brightness via `org.gnome.SettingsDaemon.Power.Screen` with fallback to `org.gnome.Mutter.DisplayConfig` (`Backlight` / `SetBacklight` for GNOME 47+ Wayland). |
| `src/services/kde_brightness.rs` | KDE PowerDevil D-Bus brightness control. |
| `src/services/touchpad_ctl.rs` | DE-agnostic touchpad enable/disable (`gsettings` on GNOME, `qdbus` on KDE). |
| `src/services/typing_watch.rs` | Keyboard monitoring for palm rejection (suppresses edge gestures and/or disables touchpad while typing). |
| `src/services/numberpad.rs`, `numberpad_layouts.rs`, `numberpad_pointer.rs` | In-process ASUS NumberPad I2C (`0x15` / `0x38`) LED packet + `uinput` keycode emitter + pointer relay. `for_product()` maps `UX3405`, `UM3402`, `B3302` to `UX3405MA_5X4` and defaults to `UNIVERSAL_4X4`. |
| `src/services/fan_hotkey.rs` | Listens for ASUS WMI fan keycodes (`204`, `425`, `467`, `482`) and emits `FanMsg::CycleFromHotkey`. |
| `src/components/` | Relm4 UI components: `widgets.rs` (`DaemonWarningLabel`), `home.rs`, `display/`, `keyboard/`, `aura/`, `animatrix/`, `touchpad/`, `audio/`, `system/`. |
| `locales/` | `en.yml`, `de.yml`, `pt-br.yml` (`rust-i18n`, fallback `"en"`). |
| `packaging/` | `.desktop` files, AppStream metainfo (`de.guido.ayuz.metainfo.xml`), `udev/99-ayuz-numberpad.rules`. |
| `assets/` | `style.css`, `trayicon.png`, `img/`, `icm/`, `presets/` (EasyEffects), `anime/` (AniMatrix GIFs). |

---

## 4. Coding Conventions & Critical Guardrails

1. **License header**: Every `.rs` file starts with the standard GPL-3.0-or-later header (`Copyright (C) 2026 Guido Philipp`).
2. **Relm4 `#[watch]` feedback-loop guard**: Relm4 re-evaluates `#[watch]` bindings on every `update`/`update_cmd`. Always start toggle/selection handlers with an early return (`if new_val == self.field { return; }`) before mutating state, writing config, or spawning commands.
3. **Never block the GTK main thread**: Offload subprocesses and blocking I/O via `commands::run_command_blocking`, `tokio::spawn`, or `sender.command(...)`.
4. **CLI flags**: GTK4 aborts on unrecognized flags before application code runs. Any new CLI flag must be parsed and stripped in `src/main.rs` before `RelmApp::with_args(gtk_args)`.
5. **Polkit / `pkexec` safety**: `pkexec_read_file` and `pkexec_write_sysfs` in `src/services/commands.rs` take `&'static str` only to prevent path/command injection. Keep those signatures restricted to static literals.
6. **Graceful feature gating**: Missing tools, missing kernel permissions (`/dev/input`, `/dev/i2c-*`, `/dev/uinput`), unavailable daemons (`asusd`, `supergfxd`, `iio-sensor-proxy`), or non-matching desktop environments must disable the control and show `DaemonWarningLabel` — never panic or fail silently.
7. **Desktop detection**: Use `commands::is_kde_desktop()`, `is_gnome_desktop()`, and `is_cosmic_desktop()` (`XDG_CURRENT_DESKTOP` case-insensitive substring check). Keep GNOME display features disabled unless explicitly requested; never expand `ColorGamutModel` to GNOME.
8. **Checklist for adding a new profile setting**:
   - Add field to `Profile` in `src/services/config.rs` with `#[serde(default = "...")]` and update `Profile::default()`.
   - Emit `LoadProfile` in `AppModel::distribute_profile` (`src/app.rs`).
   - Add widget to `widget_map` in `src/app.rs` and to `SEARCH_INDEX` in `src/search.rs`.
   - Add translation keys to **all three** files: `locales/en.yml`, `locales/de.yml`, and `locales/pt-br.yml`.
9. **Headless unit tests**: CI runs `cargo test --locked` inside `quay.io/fedora/fedora:44` with no display server, no D-Bus session/system services, and no ASUS hardware. Keep unit tests decoupled from live `/sys`, `/dev`, D-Bus, and GTK display initialization (follow the mock-directory pattern in `src/sys_paths.rs` and pure-function tests in `src/services/gnome_brightness.rs`, `src/services/edge_gestures.rs`, and `src/services/numberpad_layouts.rs`).
