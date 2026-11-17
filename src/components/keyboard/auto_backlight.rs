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
    AmbientRules, FdoScreenSaverProxy, GnomeScreenSaverProxy, KbdBacklightProxy,
    LogindManagerProxy, LogindSessionProxy, MutterDisplayPowerProxy, SensorProxyProxy, UPowerProxy,
    is_kbd_idle_dimmed,
};
use crate::services::config::AppConfig;

/// Snapshot of screen, session, power, and idle state used to gate ambient keyboard backlight changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AmbientGuardState {
    /// Display compositor reports power-save / DPMS off (`Mutter.DisplayConfig.PowerSaveMode != 0`).
    pub display_power_save: bool,
    /// Desktop screensaver / screen blank shield is active (`org.gnome.ScreenSaver` / `org.freedesktop.ScreenSaver`).
    pub screensaver_active: bool,
    /// Session reports `LockedHint == true`.
    pub session_locked: bool,
    /// Session reports `IdleHint == true`.
    pub session_idle: bool,
    /// Session reports `Active == true` (active VT/seat).
    pub session_active: bool,
    /// System is suspending (`PrepareForSleep` / `PreparingForSleep`).
    pub preparing_for_sleep: bool,
    /// Laptop lid is closed (`UPower.LidIsClosed` or `/proc/acpi/button/lid`).
    pub lid_closed: bool,
    /// Kernel DRM / backlight sysfs reports all displays powered off.
    pub sysfs_display_off: bool,
    /// In-app keyboard idle timeout (`backlight_idle`) has dimmed the keyboard.
    pub kbd_idle_dimmed: bool,
}

impl Default for AmbientGuardState {
    fn default() -> Self {
        Self {
            display_power_save: false,
            screensaver_active: false,
            session_locked: false,
            session_idle: false,
            session_active: true,
            preparing_for_sleep: false,
            lid_closed: false,
            sysfs_display_off: false,
            kbd_idle_dimmed: false,
        }
    }
}

impl AmbientGuardState {
    /// Returns `true` when the screen is off/blanked, the session is locked/idle/inactive,
    /// the laptop lid is closed, or the system is suspending.
    pub(super) fn is_screen_or_session_off(&self) -> bool {
        self.display_power_save
            || self.screensaver_active
            || self.session_locked
            || self.session_idle
            || !self.session_active
            || self.preparing_for_sleep
            || self.lid_closed
            || self.sysfs_display_off
    }

    /// Returns `true` when ambient light changes must not adjust the keyboard backlight.
    pub(super) fn should_suppress_ambient(&self) -> bool {
        self.is_screen_or_session_off() || self.kbd_idle_dimmed
    }
}

/// State for the ambient-light-based keyboard backlight automation component.
pub struct AutoBacklightModel {
    /// Whether the `iio-sensor-proxy` D-Bus service is reachable.
    sensor_available: bool,
    /// When `true`, the keyboard backlight is raised to max when lux drops below `brighten_threshold`.
    auto_brighten: bool,
    /// When `true`, the keyboard backlight is turned off when lux exceeds `dim_threshold`.
    auto_dim: bool,
    /// Ambient light level (lux) below which auto-brighten triggers.
    brighten_threshold: f64,
    /// Ambient light level (lux) above which auto-dim triggers.
    dim_threshold: f64,
    /// Sender to update [`AmbientRules`] in the running sensor loop (or send `None` to stop it).
    rules_tx: Option<watch::Sender<Option<AmbientRules>>>,
    /// Last reported ambient light level, displayed in the UI while the loop is active.
    current_lux: Option<f64>,
}

#[derive(Debug)]
pub enum AutoBacklightMsg {
    ToggleAutoBrighten(bool),
    ToggleAutoDim(bool),
    BrightenThresholdChanged(f64),
    DimThresholdChanged(f64),
    LoadProfile {
        brighten: bool,
        dim: bool,
        brighten_threshold: f64,
        dim_threshold: f64,
    },
}

#[derive(Debug)]
pub enum AutoBacklightCommandOutput {
    SensorChecked(bool),
    Error(String),
    LuxUpdated(f64),
}

/// Messages forwarded to the parent component.
///
/// [`AmbientChanged`](AutoBacklightOutput::AmbientChanged) notifies the parent that the ambient
/// automation settings were changed interactively, so the idle-timeout component can rebuild its
/// `swayidle` resume command with the new flags and thresholds (issue #39).
#[derive(Debug)]
pub enum AutoBacklightOutput {
    Error(String),
    AmbientChanged,
}

#[relm4::component(pub)]
impl Component for AutoBacklightModel {
    type Init = ();
    type Input = AutoBacklightMsg;
    type Output = AutoBacklightOutput;
    type CommandOutput = AutoBacklightCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &t!("backlight_group_title"),
            set_description: Some(&t!("backlight_group_desc")),

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                #[watch]
                set_visible: !model.sensor_available,
                set_label: &t!("backlight_sensor_missing_warning"),
            },

            add = &adw::ActionRow {
                set_title: &t!("backlight_light_level_title"),
                set_subtitle: &t!("backlight_light_level_subtitle"),

                #[watch]
                set_visible: model.sensor_available && (model.auto_brighten || model.auto_dim),

                add_suffix = &gtk::Label {
                    #[watch]
                    set_label: &match model.current_lux {
                        Some(lux) => format!("{lux:.1} lx"),
                        None => t!("backlight_no_lux").to_string(),
                    },
                    add_css_class: "numeric",
                    set_valign: gtk::Align::Center,
                },
            },

            add = &adw::SwitchRow {
                set_title: &t!("backlight_auto_on_title"),
                set_subtitle: &t!("backlight_auto_on_subtitle"),

                #[watch]
                set_sensitive: model.sensor_available,
                #[watch]
                set_active: model.auto_brighten,

                connect_active_notify[sender] => move |switch| {
                    sender.input(AutoBacklightMsg::ToggleAutoBrighten(switch.is_active()));
                },
            },

            add = &adw::ActionRow {
                set_title: &t!("backlight_threshold_on_title"),
                set_subtitle: &t!("backlight_threshold_on_subtitle"),

                #[watch]
                set_sensitive: model.sensor_available && model.auto_brighten,

                add_suffix = &gtk::SpinButton::with_range(0.0, 1000.0, 1.0) {
                    set_valign: gtk::Align::Center,

                    #[watch]
                    set_value: model.brighten_threshold,

                    connect_value_changed[sender] => move |spin| {
                        sender.input(AutoBacklightMsg::BrightenThresholdChanged(spin.value()));
                    },
                },
            },

            add = &adw::SwitchRow {
                set_title: &t!("backlight_auto_off_title"),
                set_subtitle: &t!("backlight_auto_off_subtitle"),

                #[watch]
                set_sensitive: model.sensor_available,
                #[watch]
                set_active: model.auto_dim,

                connect_active_notify[sender] => move |switch| {
                    sender.input(AutoBacklightMsg::ToggleAutoDim(switch.is_active()));
                },
            },

            add = &adw::ActionRow {
                set_title: &t!("backlight_threshold_off_title"),
                set_subtitle: &t!("backlight_threshold_off_subtitle"),

                #[watch]
                set_sensitive: model.sensor_available && model.auto_dim,

                add_suffix = &gtk::SpinButton::with_range(0.0, 1000.0, 1.0) {
                    set_valign: gtk::Align::Center,

                    #[watch]
                    set_value: model.dim_threshold,

                    connect_value_changed[sender] => move |spin| {
                        sender.input(AutoBacklightMsg::DimThresholdChanged(spin.value()));
                    },
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

        let profile = config.active_profile();
        let model = AutoBacklightModel {
            sensor_available: false,
            auto_brighten: profile.kbd_brighten_active,
            auto_dim: profile.kbd_dim_active,
            brighten_threshold: profile.kbd_brighten_threshold,
            dim_threshold: profile.kbd_dim_threshold,
            rules_tx: None,
            current_lux: None,
        };

        let widgets = view_output!();

        sender.command(move |out, shutdown| {
            shutdown
                .register(async move {
                    let available = is_sensor_available().await;
                    out.emit(AutoBacklightCommandOutput::SensorChecked(available));
                })
                .drop_on_shutdown()
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: AutoBacklightMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            AutoBacklightMsg::ToggleAutoBrighten(active) => {
                if active == self.auto_brighten {
                    return;
                }
                self.auto_brighten = active;
                AppConfig::update(|c| c.active_profile_mut().kbd_brighten_active = active);
                self.update_sensor_loop(sender.clone());
                let _ = sender.output(AutoBacklightOutput::AmbientChanged);
            }
            AutoBacklightMsg::ToggleAutoDim(active) => {
                if active == self.auto_dim {
                    return;
                }
                self.auto_dim = active;
                AppConfig::update(|c| c.active_profile_mut().kbd_dim_active = active);
                self.update_sensor_loop(sender.clone());
                let _ = sender.output(AutoBacklightOutput::AmbientChanged);
            }
            AutoBacklightMsg::BrightenThresholdChanged(value) => {
                if (value - self.brighten_threshold).abs() > f64::EPSILON {
                    self.brighten_threshold = value;
                    AppConfig::update(|c| c.active_profile_mut().kbd_brighten_threshold = value);
                    self.update_sensor_loop(sender.clone());
                    let _ = sender.output(AutoBacklightOutput::AmbientChanged);
                }
            }
            AutoBacklightMsg::DimThresholdChanged(value) => {
                if (value - self.dim_threshold).abs() > f64::EPSILON {
                    self.dim_threshold = value;
                    AppConfig::update(|c| c.active_profile_mut().kbd_dim_threshold = value);
                    self.update_sensor_loop(sender.clone());
                    let _ = sender.output(AutoBacklightOutput::AmbientChanged);
                }
            }
            AutoBacklightMsg::LoadProfile {
                brighten,
                dim,
                brighten_threshold,
                dim_threshold,
            } => {
                let changed = self.auto_brighten != brighten
                    || self.auto_dim != dim
                    || (self.brighten_threshold - brighten_threshold).abs() > f64::EPSILON
                    || (self.dim_threshold - dim_threshold).abs() > f64::EPSILON;
                self.auto_brighten = brighten;
                self.auto_dim = dim;
                self.brighten_threshold = brighten_threshold;
                self.dim_threshold = dim_threshold;
                if self.sensor_available && changed {
                    self.update_sensor_loop(sender);
                }
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: AutoBacklightCommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            AutoBacklightCommandOutput::SensorChecked(available) => {
                self.sensor_available = available;
                if available {
                    self.update_sensor_loop(sender);
                }
            }
            AutoBacklightCommandOutput::Error(e) => {
                let _ = sender.output(AutoBacklightOutput::Error(e));
            }
            AutoBacklightCommandOutput::LuxUpdated(lux) => {
                self.current_lux = Some(lux);
            }
        }
    }
}

impl AutoBacklightModel {
    fn current_rules(&self) -> Option<AmbientRules> {
        if self.auto_brighten || self.auto_dim {
            Some(AmbientRules {
                auto_brighten: self.auto_brighten,
                brighten_threshold: self.brighten_threshold,
                auto_dim: self.auto_dim,
                dim_threshold: self.dim_threshold,
            })
        } else {
            None
        }
    }

    fn update_sensor_loop(&mut self, sender: ComponentSender<Self>) {
        if !self.sensor_available {
            return;
        }

        match self.current_rules() {
            Some(rules) => {
                if let Some(tx) = &self.rules_tx
                    && tx.send(Some(rules)).is_ok()
                {
                    return;
                }
                self.rules_tx = Some(start_sensor_loop(rules, &sender));
            }
            None => {
                if let Some(tx) = self.rules_tx.take() {
                    let _ = tx.send(None);
                }
                self.current_lux = None;
            }
        }
    }
}

/// Returns `true` if the `iio-sensor-proxy` D-Bus service is reachable and reports an ambient light sensor.
async fn is_sensor_available() -> bool {
    let conn = match zbus::Connection::system().await {
        Ok(c) => c,
        Err(_) => return false,
    };
    let proxy = match SensorProxyProxy::new(&conn).await {
        Ok(p) => p,
        Err(_) => return false,
    };
    proxy.has_ambient_light().await.is_ok()
}

/// Resolves a logind [`LogindSessionProxy`] for the current desktop session, used to detect
/// whether the session is locked, idle, or inactive.
///
/// The session is looked up by `XDG_SESSION_ID` first, falling back to the running process'
/// PID. Returns `None` if logind is unavailable or the session cannot be resolved.
async fn resolve_session_proxy(conn: &zbus::Connection) -> Option<LogindSessionProxy<'static>> {
    let manager = LogindManagerProxy::new(conn).await.ok()?;

    let path = match std::env::var("XDG_SESSION_ID") {
        Ok(id) if !id.is_empty() => match manager.get_session(&id).await {
            Ok(p) => Some(p),
            Err(_) => manager.get_session_by_pid(std::process::id()).await.ok(),
        },
        _ => manager.get_session_by_pid(std::process::id()).await.ok(),
    }?;

    LogindSessionProxy::builder(conn)
        .path(path)
        .ok()?
        .build()
        .await
        .ok()
}

/// Refreshes the synchronous/sysfs and live D-Bus properties on `guard`.
async fn refresh_live_guard_state(
    guard: &mut AmbientGuardState,
    session: Option<&LogindSessionProxy<'_>>,
    logind_manager: Option<&LogindManagerProxy<'_>>,
    upower: Option<&UPowerProxy<'_>>,
    mutter_power: Option<&MutterDisplayPowerProxy<'_>>,
    gnome_ss: Option<&GnomeScreenSaverProxy<'_>>,
    fdo_ss: Option<&FdoScreenSaverProxy<'_>>,
) {
    guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
    guard.kbd_idle_dimmed = is_kbd_idle_dimmed();

    let upower_lid = match upower {
        Some(p) => p.lid_is_closed().await.unwrap_or(false),
        None => false,
    };
    guard.lid_closed = upower_lid || crate::sys_paths::is_lid_closed();

    if let Some(s) = session {
        if let Ok(locked) = s.locked_hint().await {
            guard.session_locked = locked;
        }
        if let Ok(idle) = s.idle_hint().await {
            guard.session_idle = idle;
        }
        if let Ok(active) = s.active().await {
            guard.session_active = active;
        }
    }
    if let Some(m) = logind_manager
        && let Ok(prep) = m.preparing_for_sleep().await
    {
        guard.preparing_for_sleep = prep;
    }
    if let Some(mp) = mutter_power
        && let Ok(mode) = mp.power_save_mode().await
    {
        guard.display_power_save = mode != 0;
    }
    let gnome_ss_active = match gnome_ss {
        Some(gs) => gs.get_active().await.ok(),
        None => None,
    };
    let fdo_ss_active = match fdo_ss {
        Some(fs) => fs.get_active().await.ok(),
        None => None,
    };
    if gnome_ss_active.is_some() || fdo_ss_active.is_some() {
        guard.screensaver_active =
            gnome_ss_active.unwrap_or(false) || fdo_ss_active.unwrap_or(false);
    }
}

/// Decides the target keyboard backlight brightness for a given ambient `level`,
/// returning `Some(target)` only when the level differs from `current_brightness`.
pub(super) fn decide_ambient_brightness(
    level: f64,
    rules: AmbientRules,
    current_brightness: i32,
    max_brightness: i32,
) -> Option<i32> {
    let max_b = max_brightness.max(1);
    if rules.auto_brighten && level < rules.brighten_threshold && current_brightness != max_b {
        Some(max_b)
    } else if rules.auto_dim && level > rules.dim_threshold && current_brightness != 0 {
        Some(0)
    } else {
        None
    }
}

/// Applies auto-brighten/dim rules for a given lux reading via `UPower.KbdBacklight`
/// and returns the resulting brightness level.
///
/// Queries the actual hardware brightness when available so manual hotkey changes or
/// idle-timeout dimming never leave `current_brightness` stale, and only sends a
/// `SetBrightness` D-Bus call when brightness would actually change.
async fn light_sensor_logic(
    level: f64,
    rules: AmbientRules,
    mut current_brightness: i32,
    max_brightness: i32,
    kbd_proxy: Option<&KbdBacklightProxy<'_>>,
) -> i32 {
    if let Some(kbd) = kbd_proxy
        && let Ok(hw_cur) = kbd.get_brightness().await
    {
        current_brightness = hw_cur;
    }

    if let Some(target) =
        decide_ambient_brightness(level, rules, current_brightness, max_brightness)
        && let Some(kbd) = kbd_proxy
    {
        match kbd.set_brightness(target).await {
            Ok(()) => current_brightness = target,
            Err(e) => tracing::warn!("UPower SetBrightness({target}) failed: {e}"),
        }
    }
    current_brightness
}

/// Ensures the keyboard backlight is turned off when the screen turns off / locks / suspends.
async fn turn_off_kbd_on_screen_off(
    current_brightness: &mut i32,
    kbd_proxy: Option<&KbdBacklightProxy<'_>>,
) {
    let Some(kbd) = kbd_proxy else {
        return;
    };
    let hw_cur = kbd.get_brightness().await.unwrap_or(*current_brightness);
    if hw_cur != 0 {
        match kbd.set_brightness(0).await {
            Ok(()) => *current_brightness = 0,
            Err(e) => tracing::warn!("UPower SetBrightness(0) on screen-off failed: {e}"),
        }
    } else {
        *current_brightness = 0;
    }
}

/// Handles a transition in [`AmbientGuardState`]:
/// - When transitioning to screen/session off, turns the keyboard backlight off (`0`).
/// - When transitioning back to screen/session on (and not suppressed by keyboard idle timeout),
///   immediately re-evaluates the latest ambient light reading.
async fn handle_guard_transition(
    was_off: bool,
    guard: &AmbientGuardState,
    last_level: f64,
    rules: AmbientRules,
    current_brightness: &mut i32,
    max_brightness: i32,
    kbd_proxy: Option<&KbdBacklightProxy<'_>>,
) {
    let is_off = guard.is_screen_or_session_off();
    if !was_off && is_off {
        turn_off_kbd_on_screen_off(current_brightness, kbd_proxy).await;
    } else if was_off && !guard.should_suppress_ambient() && last_level >= 0.0 {
        *current_brightness = light_sensor_logic(
            last_level,
            rules,
            -1,
            max_brightness,
            kbd_proxy,
        )
        .await;
    }
}

/// Spawns a background task that monitors the ambient light sensor and adjusts the keyboard
/// backlight in response to lux changes while the screen is on and the session is active.
///
/// A 3-lux hysteresis is applied - changes smaller than 3 lux are ignored to avoid
/// flickering from sensor noise. Rule updates are delivered live via the returned
/// [`watch::Sender`]; sending `None` stops the task and releases the sensor claim.
fn start_sensor_loop(
    initial_rules: AmbientRules,
    sender: &ComponentSender<AutoBacklightModel>,
) -> watch::Sender<Option<AmbientRules>> {
    let (tx, mut rx) = watch::channel(Some(initial_rules));
    let out = sender.command_sender().clone();

    tokio::spawn(async move {
        let mut rules = initial_rules;
        let conn = match zbus::Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                out.emit(AutoBacklightCommandOutput::Error(
                    t!("error_dbus_connect", error = e.to_string()).to_string(),
                ));
                return;
            }
        };

        let proxy = match SensorProxyProxy::new(&conn).await {
            Ok(p) => p,
            Err(e) => {
                out.emit(AutoBacklightCommandOutput::Error(
                    t!("error_sensor_proxy", error = e.to_string()).to_string(),
                ));
                return;
            }
        };

        if let Err(e) = proxy.claim_light().await {
            out.emit(AutoBacklightCommandOutput::Error(
                t!("error_claim_light", error = e.to_string()).to_string(),
            ));
            return;
        }

        let kbd_proxy = match KbdBacklightProxy::new(&conn).await {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("UPower KbdBacklight proxy unavailable: {e}");
                None
            }
        };
        let max_brightness = match kbd_proxy.as_ref() {
            Some(p) => p
                .get_max_brightness()
                .await
                .ok()
                .filter(|&v| v > 0)
                .unwrap_or(3),
            None => 3,
        };
        let mut kbd_changed_stream = match &kbd_proxy {
            Some(p) => p.receive_brightness_changed().await.ok(),
            None => None,
        };

        let logind_manager = LogindManagerProxy::new(&conn).await.ok();
        let mut sleep_stream = match &logind_manager {
            Some(m) => m.receive_prepare_for_sleep().await.ok(),
            None => None,
        };

        let session = resolve_session_proxy(&conn).await;
        let mut lock_stream = match &session {
            Some(proxy) => Some(proxy.receive_locked_hint_changed().await),
            None => None,
        };
        let mut idle_hint_stream = match &session {
            Some(proxy) => Some(proxy.receive_idle_hint_changed().await),
            None => None,
        };
        let mut active_stream = match &session {
            Some(proxy) => Some(proxy.receive_active_changed().await),
            None => None,
        };

        let upower_proxy = UPowerProxy::new(&conn).await.ok();
        let mut lid_stream = match &upower_proxy {
            Some(p) => Some(p.receive_lid_is_closed_changed().await),
            None => None,
        };

        let session_conn = zbus::Connection::session().await.ok();
        let mutter_power = match &session_conn {
            Some(sc) => MutterDisplayPowerProxy::new(sc).await.ok(),
            None => None,
        };
        let mut mutter_power_stream = match &mutter_power {
            Some(mp) => Some(mp.receive_power_save_mode_changed().await),
            None => None,
        };

        let gnome_ss = match &session_conn {
            Some(sc) => GnomeScreenSaverProxy::new(sc).await.ok(),
            None => None,
        };
        let mut gnome_ss_stream = match &gnome_ss {
            Some(gs) => gs.receive_active_changed().await.ok(),
            None => None,
        };

        let fdo_ss = match &session_conn {
            Some(sc) => FdoScreenSaverProxy::new(sc).await.ok(),
            None => None,
        };
        let mut fdo_ss_stream = match &fdo_ss {
            Some(fs) => fs.receive_active_changed().await.ok(),
            None => None,
        };

        let mut guard = AmbientGuardState::default();
        refresh_live_guard_state(
            &mut guard,
            session.as_ref(),
            logind_manager.as_ref(),
            upower_proxy.as_ref(),
            mutter_power.as_ref(),
            gnome_ss.as_ref(),
            fdo_ss.as_ref(),
        )
        .await;

        let level_stream = proxy.receive_light_level_changed().await;
        let mut current_brightness: i32 = match kbd_proxy.as_ref() {
            Some(p) => p.get_brightness().await.unwrap_or(-1),
            None => -1,
        };
        let mut last_level: f64 = -100.0;

        match proxy.light_level().await {
            Ok(level) => {
                last_level = level;
                if !guard.should_suppress_ambient() {
                    current_brightness = light_sensor_logic(
                        level,
                        rules,
                        current_brightness,
                        max_brightness,
                        kbd_proxy.as_ref(),
                    )
                    .await;
                }
                out.emit(AutoBacklightCommandOutput::LuxUpdated(level));
            }
            Err(e) => tracing::warn!(
                "{}",
                t!("backlight_sensor_init_error", error = e.to_string())
            ),
        }

        tokio::pin!(level_stream);

        loop {
            tokio::select! {
                res = rx.changed() => {
                    if res.is_err() {
                        break;
                    }
                    let Some(new_rules) = *rx.borrow_and_update() else {
                        break;
                    };
                    rules = new_rules;
                    refresh_live_guard_state(
                        &mut guard,
                        session.as_ref(),
                        logind_manager.as_ref(),
                        upower_proxy.as_ref(),
                        mutter_power.as_ref(),
                        gnome_ss.as_ref(),
                        fdo_ss.as_ref(),
                    )
                    .await;
                    if !guard.should_suppress_ambient() && last_level >= 0.0 {
                        current_brightness = light_sensor_logic(
                            last_level,
                            rules,
                            current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_kbd = async {
                    match kbd_changed_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(sig) = changed_kbd
                        && let Ok(args) = sig.args()
                    {
                        current_brightness = args.value;
                    }
                }
                changed_mutter = async {
                    match mutter_power_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(change) = changed_mutter
                        && let Ok(mode) = change.get().await
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.display_power_save = mode != 0;
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_gnome_ss = async {
                    match gnome_ss_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(sig) = changed_gnome_ss
                        && let Ok(args) = sig.args()
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.screensaver_active = args.new_value;
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_fdo_ss = async {
                    match fdo_ss_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(sig) = changed_fdo_ss
                        && let Ok(args) = sig.args()
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.screensaver_active = args.new_value;
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_lock = async {
                    match lock_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(change) = changed_lock
                        && let Ok(locked) = change.get().await
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.session_locked = locked;
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_idle_hint = async {
                    match idle_hint_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(change) = changed_idle_hint
                        && let Ok(idle) = change.get().await
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.session_idle = idle;
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_active = async {
                    match active_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(change) = changed_active
                        && let Ok(active) = change.get().await
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.session_active = active;
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_sleep = async {
                    match sleep_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(sig) = changed_sleep
                        && let Ok(args) = sig.args()
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.preparing_for_sleep = args.start;
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                changed_lid = async {
                    match lid_stream.as_mut() {
                        Some(s) => s.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(change) = changed_lid
                        && let Ok(closed) = change.get().await
                    {
                        let was_off = guard.is_screen_or_session_off();
                        guard.lid_closed = closed || crate::sys_paths::is_lid_closed();
                        guard.sysfs_display_off = !crate::sys_paths::is_any_display_powered();
                        guard.kbd_idle_dimmed = is_kbd_idle_dimmed();
                        handle_guard_transition(
                            was_off,
                            &guard,
                            last_level,
                            rules,
                            &mut current_brightness,
                            max_brightness,
                            kbd_proxy.as_ref(),
                        )
                        .await;
                    }
                }
                maybe = level_stream.next() => {
                    if let Some(changed) = maybe {
                        match changed.get().await {
                            Ok(level) => {
                                if (level - last_level).abs() < 3.0 {
                                    continue;
                                }
                                last_level = level;
                                refresh_live_guard_state(
                                    &mut guard,
                                    session.as_ref(),
                                    logind_manager.as_ref(),
                                    upower_proxy.as_ref(),
                                    mutter_power.as_ref(),
                                    gnome_ss.as_ref(),
                                    fdo_ss.as_ref(),
                                )
                                .await;
                                if !guard.should_suppress_ambient() {
                                    current_brightness = light_sensor_logic(
                                        level,
                                        rules,
                                        current_brightness,
                                        max_brightness,
                                        kbd_proxy.as_ref(),
                                    )
                                    .await;
                                }
                                out.emit(AutoBacklightCommandOutput::LuxUpdated(level));
                            }
                            Err(e) => tracing::warn!(
                                "{}",
                                t!("backlight_sensor_read_error", error = e.to_string())
                            ),
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        let _ = proxy.release_light().await;
    });

    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_rules() -> AmbientRules {
        AmbientRules {
            auto_brighten: true,
            brighten_threshold: 12.0,
            auto_dim: true,
            dim_threshold: 35.0,
        }
    }

    #[test]
    fn test_decide_ambient_brightness_dynamic_max() {
        let rules = test_rules();
        assert_eq!(decide_ambient_brightness(5.0, rules, -1, 255), Some(255));
        assert_eq!(decide_ambient_brightness(5.0, rules, 255, 255), None);
        assert_eq!(decide_ambient_brightness(5.0, rules, -1, 1), Some(1));

        // Non-positive max brightness clamps safely to 1
        let brighten_only = AmbientRules {
            auto_dim: false,
            ..rules
        };
        assert_eq!(
            decide_ambient_brightness(5.0, brighten_only, 0, 0),
            Some(1)
        );
    }

    #[test]
    fn test_decide_ambient_brightness_deadband_and_dim() {
        let rules = test_rules();
        // Inside deadband [12.0, 35.0] -> no change
        assert_eq!(decide_ambient_brightness(20.0, rules, 3, 3), None);
        assert_eq!(decide_ambient_brightness(12.0, rules, 0, 3), None);
        assert_eq!(decide_ambient_brightness(35.0, rules, 3, 3), None);

        // Above dim threshold -> turn off
        assert_eq!(decide_ambient_brightness(40.0, rules, 3, 3), Some(0));
        // Already off -> no redundant call
        assert_eq!(decide_ambient_brightness(40.0, rules, 0, 3), None);
    }

    #[test]
    fn test_ambient_guard_state_suppression() {
        let mut guard = AmbientGuardState::default();
        assert!(!guard.is_screen_or_session_off());
        assert!(!guard.should_suppress_ambient());

        // Mutter DPMS power save
        guard.display_power_save = true;
        assert!(guard.is_screen_or_session_off());
        assert!(guard.should_suppress_ambient());
        guard.display_power_save = false;

        // ScreenSaver / blanking shield active
        guard.screensaver_active = true;
        assert!(guard.is_screen_or_session_off());
        guard.screensaver_active = false;

        // Session locked
        guard.session_locked = true;
        assert!(guard.is_screen_or_session_off());
        guard.session_locked = false;

        // Session idle hint
        guard.session_idle = true;
        assert!(guard.is_screen_or_session_off());
        guard.session_idle = false;

        // Session on inactive VT
        guard.session_active = false;
        assert!(guard.is_screen_or_session_off());
        guard.session_active = true;

        // System suspending
        guard.preparing_for_sleep = true;
        assert!(guard.is_screen_or_session_off());
        guard.preparing_for_sleep = false;

        // Laptop lid closed
        guard.lid_closed = true;
        assert!(guard.is_screen_or_session_off());
        guard.lid_closed = false;

        // Kernel DRM / backlight sysfs display off
        guard.sysfs_display_off = true;
        assert!(guard.is_screen_or_session_off());
        guard.sysfs_display_off = false;

        // Keyboard idle timeout dimmed: suppresses ambient evaluation without counting as screen-off
        guard.kbd_idle_dimmed = true;
        assert!(!guard.is_screen_or_session_off());
        assert!(guard.should_suppress_ambient());
    }
}
