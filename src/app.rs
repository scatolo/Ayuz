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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::components::animatrix::{AnimatrixModel, AnimatrixMsg};
use crate::components::audio::volume::VolumeMsg;
use crate::components::audio::{SoundModesModel, SoundModesMsg, VolumeModel};
use crate::components::aura::aura_page::AuraPageOutput;
use crate::components::aura::{AuraPageModel, AuraPageMsg};
use crate::components::display::color_gamut::ColorGamutMsg;
use crate::components::display::oled_care::OledCareMsg;
use crate::components::display::oled_dimming::OledDimmingMsg;
use crate::components::display::refresh_rate::RefreshRateMsg;
use crate::components::display::target_mode::TargetModeMsg;
use crate::components::display::{
    ColorGamutModel, OledCareModel, OledDimmingModel, RefreshRateModel, TargetModeModel,
};
use crate::components::home::{HomeModel, HomeMsg, HomeOutput};
use crate::components::keyboard::auto_backlight::{AutoBacklightMsg, AutoBacklightOutput};
use crate::components::keyboard::backlight_idle::BacklightIdleMsg;
use crate::components::keyboard::fn_key::FnKeyMsg;
use crate::components::keyboard::{AutoBacklightModel, BacklightIdleModel, FnKeyModel};
use crate::components::system::apu_mem::{ApuMemModel, ApuMemMsg, ApuMemOutput};
use crate::components::system::armoury::{ArmouryModel, ArmouryMsg};
use crate::components::system::battery::{BatteryModel, BatteryMsg, BatteryOutput};
use crate::components::system::diagnostics::{DiagnosticsModel, DiagnosticsMsg, DiagnosticsOutput};
use crate::components::system::fan::{FanModel, FanMsg, FanOutput};
use crate::components::system::gpu::{GpuModel, GpuMsg, GpuOutput};
use crate::components::touchpad::gestures::GesturesMsg;
use crate::components::touchpad::numberpad::{NumberpadModel, NumberpadMsg, NumberpadOutput};
use crate::components::touchpad::touchpad::TouchpadMsg;
use crate::components::touchpad::typing::TypingMsg;
use crate::components::touchpad::{GesturesModel, TouchpadModel, TypingModel};
use crate::search::{sorted_nav_items, SearchVisibility};
use crate::services::commands::{is_cosmic_desktop, is_gnome_desktop, is_kde_desktop};
use crate::services::config::AppConfig;
use crate::services::dbus::FanProfile;
use crate::services::dbus_animatrix::{detect_animatrix_hardware, AnimatrixHardwareType};
use crate::services::power_auto::{self, PowerAutoSettings};
use crate::tray;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;
use std::rc::Rc;

/// Builds a Linux *abstract* (per-user-namespace) Unix socket address with the
/// given name. Returns None on non-Linux platforms where abstract sockets
/// are unsupported. Abstract addresses live in a separate namespace, do not
/// touch the filesystem, and are cleaned up automatically when the process
/// exits - exactly what we want for a tiny CLI -> running-GUI handshake.
pub(crate) fn abstract_socket_addr(name: &str) -> Option<std::os::unix::net::SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    std::os::unix::net::SocketAddr::from_abstract_name(name).ok()
}

macro_rules! launch_component {
    ($type:ty, $sender:expr) => {
        <$type>::builder()
            .launch(())
            .forward($sender.input_sender(), |msg: String| AppMsg::Error(msg))
    };
}

#[derive(Clone, Copy)]
enum AppPage {
    Home,
    Display,
    Keyboard,
    Aura,
    Animatrix,
    Touchpad,
    Audio,
    System,
    Search,
}

impl AppPage {
    fn as_str(self) -> &'static str {
        match self {
            AppPage::Home => "home",
            AppPage::Display => "display",
            AppPage::Keyboard => "keyboard",
            AppPage::Aura => "aura",
            AppPage::Animatrix => "animatrix",
            AppPage::Touchpad => "touchpad",
            AppPage::Audio => "audio",
            AppPage::System => "system",
            AppPage::Search => "search",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportSlot {
    Aura,
    Gpu,
    ApuMem,
    Numberpad,
}

#[derive(Debug, Clone)]
struct HardwareSupportState {
    animatrix_supported: bool,
    aura_supported: Option<bool>,
    gpu_supported: Option<bool>,
    apu_mem_supported: Option<bool>,
    numberpad_supported: Option<bool>,
    kde_available: bool,
    cosmic_available: bool,
    gnome_available: bool,
}

struct PowerAutoControls {
    group: adw::PreferencesGroup,
    enabled_row: adw::SwitchRow,
    ac_combo: adw::ComboRow,
    battery_combo: adw::ComboRow,
    profile_ids: Rc<RefCell<Vec<Option<String>>>>,
    suppress_signals: Rc<Cell<bool>>,
}

impl PowerAutoControls {
    fn new(sender: &ComponentSender<AppModel>, config: &AppConfig) -> Self {
        let group = adw::PreferencesGroup::new();
        group.set_title(&gtk4::glib::markup_escape_text(&t!("power_auto_group_title")));
        group.set_description(Some(&t!("power_auto_group_desc")));

        let enabled_row = adw::SwitchRow::new();
        enabled_row.set_title(&t!("power_auto_enable_title"));

        let ac_combo = adw::ComboRow::new();
        ac_combo.set_title(&t!("power_auto_ac_title"));

        let battery_combo = adw::ComboRow::new();
        battery_combo.set_title(&t!("power_auto_battery_title"));

        group.add(&enabled_row);
        group.add(&ac_combo);
        group.add(&battery_combo);

        let controls = Self {
            group,
            enabled_row,
            ac_combo,
            battery_combo,
            profile_ids: Rc::new(RefCell::new(Vec::new())),
            suppress_signals: Rc::new(Cell::new(false)),
        };
        controls.sync_from_config(config);

        let sender_en = sender.clone();
        let suppress_en = controls.suppress_signals.clone();
        controls.enabled_row.connect_active_notify(move |row| {
            if suppress_en.get() {
                return;
            }
            sender_en.input(AppMsg::TogglePowerAutoSwitch(row.is_active()));
        });

        let sender_ac = sender.clone();
        let suppress_ac = controls.suppress_signals.clone();
        let ids_ac = controls.profile_ids.clone();
        controls.ac_combo.connect_selected_notify(move |row| {
            if suppress_ac.get() {
                return;
            }
            let idx = row.selected() as usize;
            let id = ids_ac.borrow().get(idx).cloned().flatten();
            sender_ac.input(AppMsg::SetAcProfile(id));
        });

        let sender_bat = sender.clone();
        let suppress_bat = controls.suppress_signals.clone();
        let ids_bat = controls.profile_ids.clone();
        controls.battery_combo.connect_selected_notify(move |row| {
            if suppress_bat.get() {
                return;
            }
            let idx = row.selected() as usize;
            let id = ids_bat.borrow().get(idx).cloned().flatten();
            sender_bat.input(AppMsg::SetBatteryProfile(id));
        });

        controls
    }

    fn sync_from_config(&self, config: &AppConfig) {
        self.suppress_signals.set(true);

        self.enabled_row.set_active(config.power_auto_switch_enabled);
        self.ac_combo.set_sensitive(config.power_auto_switch_enabled);
        self.battery_combo
            .set_sensitive(config.power_auto_switch_enabled);

        let mut labels = vec![t!("power_auto_none").to_string()];
        let mut ids: Vec<Option<String>> = vec![None];
        for profile in &config.profiles {
            labels.push(profile.name.clone());
            ids.push(Some(profile.id.clone()));
        }

        let str_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
        self.ac_combo
            .set_model(Some(&gtk4::StringList::new(&str_refs)));
        self.battery_combo
            .set_model(Some(&gtk4::StringList::new(&str_refs)));

        let ac_idx = index_for_profile(&ids, config.ac_profile_id.as_deref());
        let bat_idx = index_for_profile(&ids, config.battery_profile_id.as_deref());
        self.ac_combo.set_selected(ac_idx);
        self.battery_combo.set_selected(bat_idx);

        *self.profile_ids.borrow_mut() = ids;
        self.suppress_signals.set(false);
    }

    fn set_enabled(&self, enabled: bool) {
        self.suppress_signals.set(true);
        self.enabled_row.set_active(enabled);
        self.ac_combo.set_sensitive(enabled);
        self.battery_combo.set_sensitive(enabled);
        self.suppress_signals.set(false);
    }
}

fn index_for_profile(ids: &[Option<String>], target: Option<&str>) -> u32 {
    match target {
        Some(t) => ids
            .iter()
            .position(|entry| entry.as_deref() == Some(t))
            .unwrap_or(0) as u32,
        None => 0,
    }
}

#[derive(Debug)]
pub enum AppMsg {
    ShowWindow,
    QuitApp,
    Error(String),
    Toast(String),
    SetLanguage(String),
    ToggleAutostart(bool),
    ToggleHideUnsupported(bool),
    TogglePowerAutoSwitch(bool),
    SetAcProfile(Option<String>),
    SetBatteryProfile(Option<String>),
    ActivateProfile(String),
    ProfilesChanged,
    SetFanProfileFromTray(FanProfile),
    ToggleBatteryMaintenanceFromTray,
    FanStateChanged {
        profile: FanProfile,
        asusd_available: bool,
    },
    BatteryStateChanged {
        limit: u8,
        asusd_available: bool,
    },
    HardwareSupportUpdated {
        slot: SupportSlot,
        supported: bool,
    },
    LegacyMigrationAccepted,
    LegacyMigrationDeclined,
    TriggerManualMigration,
    /// Ambient keyboard-backlight settings changed; rebuild the idle-timeout resume command.
    AmbientBacklightChanged,
}

pub struct AppModel {
    start_hidden: bool,
    window: gtk4::glib::WeakRef<adw::ApplicationWindow>,
    toast_overlay: adw::ToastOverlay,
    tray: ksni::Handle<tray::AyuzTray>,
    tray_fan_profile: FanProfile,
    tray_fan_available: bool,
    tray_battery_limit: u8,
    tray_battery_available: bool,
    sidebar_list: gtk4::ListBox,
    sidebar_rows: HashMap<&'static str, gtk4::ListBoxRow>,
    content_stack: adw::ViewStack,
    search_visibility: Rc<RefCell<SearchVisibility>>,
    hide_unsupported_hardware: bool,
    hardware_support: HardwareSupportState,
    power_auto_tx: tokio::sync::watch::Sender<PowerAutoSettings>,
    power_auto_controls: PowerAutoControls,
    home: Controller<HomeModel>,
    apu_mem: Controller<ApuMemModel>,
    armoury: Controller<ArmouryModel>,
    battery: Controller<BatteryModel>,
    diagnostics: Controller<DiagnosticsModel>,
    fan: Controller<FanModel>,
    gpu: Controller<GpuModel>,
    refresh_rate: Controller<RefreshRateModel>,
    oled_dimming: Controller<OledDimmingModel>,
    target_mode: Controller<TargetModeModel>,
    oled_care: Controller<OledCareModel>,
    color_gamut: Controller<ColorGamutModel>,
    aura: Controller<AuraPageModel>,
    animatrix: Controller<AnimatrixModel>,
    fn_key: Controller<FnKeyModel>,
    gestures: Controller<GesturesModel>,
    numberpad: Controller<NumberpadModel>,
    touchpad: Controller<TouchpadModel>,
    typing: Controller<TypingModel>,
    auto_backlight: Controller<AutoBacklightModel>,
    backlight_idle: Controller<BacklightIdleModel>,
    sound_modes: Controller<SoundModesModel>,
    volume_widget: Controller<VolumeModel>,
}

impl AppModel {
    fn build_tray_state(&self) -> tray::TrayState {
        let config = AppConfig::load();
        let profiles = config
            .profiles
            .iter()
            .map(|p| tray::TrayProfileEntry {
                id: p.id.clone(),
                name: p.name.clone(),
                active: p.id == config.active_profile_id,
            })
            .collect();
        tray::TrayState {
            profiles,
            fan_profile: self.tray_fan_profile,
            fan_available: self.tray_fan_available,
            battery_limit: self.tray_battery_limit,
            battery_available: self.tray_battery_available,
        }
    }

    fn sync_tray_state(&self) {
        let state = self.build_tray_state();
        self.tray.update(move |tray| {
            tray.state = state;
        });
    }

    fn send_power_auto_settings(&self, config: &AppConfig) {
        let next = PowerAutoSettings::from(config);
        self.power_auto_tx.send_if_modified(|cur| {
            if *cur == next {
                false
            } else {
                *cur = next;
                true
            }
        });
    }

    fn apply_hardware_visibility(&self) {
        let hide = self.hide_unsupported_hardware;
        let animatrix_visible = !hide || self.hardware_support.animatrix_supported;
        let aura_visible = !hide || self.hardware_support.aura_supported.unwrap_or(true);
        let gpu_visible = !hide || self.hardware_support.gpu_supported.unwrap_or(true);
        let apu_mem_visible = !hide || self.hardware_support.apu_mem_supported.unwrap_or(true);
        let numberpad_visible = !hide || self.hardware_support.numberpad_supported.unwrap_or(true);
        let kde_visible = !hide || self.hardware_support.kde_available;
        let panel_ctrl_visible =
            !hide || self.hardware_support.kde_available || self.hardware_support.cosmic_available;
        let gnome_visible = !hide || self.hardware_support.gnome_available;

        if let Some(row) = self.sidebar_rows.get("animatrix") {
            row.set_visible(animatrix_visible);
        }
        if let Some(row) = self.sidebar_rows.get("aura") {
            row.set_visible(aura_visible);
        }
        self.sidebar_list.invalidate_headers();

        self.gpu.widget().set_visible(gpu_visible);
        self.apu_mem.widget().set_visible(apu_mem_visible);
        self.numberpad.widget().set_visible(numberpad_visible);
        self.oled_dimming.widget().set_visible(kde_visible);
        self.target_mode.widget().set_visible(kde_visible);
        self.color_gamut.widget().set_visible(kde_visible);
        self.oled_care
            .sender()
            .emit(OledCareMsg::SetHideUnsupported(hide));

        {
            let mut vis = self.search_visibility.borrow_mut();
            vis.hidden_pages.clear();
            vis.hidden_components.clear();
            vis.hidden_title_keys.clear();

            if !animatrix_visible {
                vis.hidden_pages.insert("animatrix");
                vis.hidden_components.insert("animatrix");
            }
            if !aura_visible {
                vis.hidden_pages.insert("aura");
                vis.hidden_components.insert("aura");
            }
            if !gpu_visible {
                vis.hidden_components.insert("gpu");
            }
            if !apu_mem_visible {
                vis.hidden_components.insert("apu_mem");
            }
            if !numberpad_visible {
                vis.hidden_components.insert("numberpad");
            }
            if !kde_visible {
                vis.hidden_components.insert("oled_dimming");
                vis.hidden_components.insert("target_mode");
                vis.hidden_components.insert("color_gamut");
                vis.hidden_title_keys.insert("oled_care_pixel_refresh_title");
            }
            if !panel_ctrl_visible {
                vis.hidden_title_keys.insert("oled_care_panel_autohide_title");
                vis.hidden_title_keys.insert("oled_care_transparency_title");
            }
            if !gnome_visible {
                vis.hidden_title_keys.insert("oled_care_screen_blank_title");
                vis.hidden_title_keys.insert("oled_care_ambient_brightness_title");
            }

            if let Some(current) = self.content_stack.visible_child_name()
                && vis.hidden_pages.contains(current.as_str())
                && let Some(home_row) = self.sidebar_rows.get("home")
            {
                self.sidebar_list.select_row(Some(home_row));
            }
        }
    }
    /// Broadcasts the active profile's settings to every sub-component.
    ///
    /// Each `LoadProfile` message is the canonical entry point that tells a
    /// component to overwrite its in-memory state with the values from
    /// `profile`. Grouped per page so the call sites stay readable.
    fn distribute_profile(&self, p: &crate::services::config::Profile) {
        // Display
        self.fan.sender().emit(FanMsg::LoadProfile(FanProfile::from(p.fan_profile)));
        self.refresh_rate.sender().emit(RefreshRateMsg::LoadProfile(p.display_refresh_hz));
        self.oled_dimming.sender().emit(OledDimmingMsg::LoadProfile(p.oled_dc_dimming));
        self.target_mode.sender().emit(TargetModeMsg::LoadProfile(p.target_mode_active));
        self.color_gamut.sender().emit(ColorGamutMsg::LoadProfile(p.color_profile_index));
        self.oled_care.sender().emit(OledCareMsg::LoadProfile {
            pixel_refresh: p.oled_care_pixel_refresh,
            panel_autohide: p.oled_care_panel_autohide,
            transparency: p.oled_care_transparency,
            gnome_screen_blank_secs: p.gnome_screen_blank_secs,
            gnome_ambient_brightness: p.gnome_ambient_brightness,
        });

        // Audio
        self.sound_modes.sender().emit(SoundModesMsg::LoadProfile(p.audio_profile));
        self.volume_widget.sender().emit(VolumeMsg::LoadProfile(p.volume));

        // Keyboard & input
        self.auto_backlight.sender().emit(AutoBacklightMsg::LoadProfile {
            brighten: p.kbd_brighten_active,
            dim: p.kbd_dim_active,
            brighten_threshold: p.kbd_brighten_threshold,
            dim_threshold: p.kbd_dim_threshold,
        });
        self.backlight_idle.sender().emit(BacklightIdleMsg::LoadProfile {
            mode: p.kbd_timeout_mode,
            ac_index: p.kbd_timeout_battery_ac_index,
            battery_index: p.kbd_timeout_battery_only_index,
        });
        self.aura.sender().emit(AuraPageMsg::LoadProfile {
            mode: p.aura_mode,
            zone: p.aura_zone,
            brightness: p.aura_brightness,
            colour_r: p.aura_colour_r,
            colour_g: p.aura_colour_g,
            colour_b: p.aura_colour_b,
            colour2_r: p.aura_colour2_r,
            colour2_g: p.aura_colour2_g,
            colour2_b: p.aura_colour2_b,
            speed: p.aura_speed.clone(),
            direction: p.aura_direction.clone(),
        });
        self.animatrix.sender().emit(AnimatrixMsg::LoadProfile {
            enable_display: p.animatrix_enable_display,
            brightness: p.animatrix_brightness,
            builtins_enabled: p.animatrix_builtins_enabled,
            boot_anim: p.animatrix_boot_anim.clone(),
            awake_anim: p.animatrix_awake_anim.clone(),
            sleep_anim: p.animatrix_sleep_anim.clone(),
            shutdown_anim: p.animatrix_shutdown_anim.clone(),
            off_unplugged: p.animatrix_off_when_unplugged,
            off_suspended: p.animatrix_off_when_suspended,
            off_lid_closed: p.animatrix_off_when_lid_closed,
        });
        self.touchpad.sender().emit(TouchpadMsg::LoadProfile(p.touchpad_active));
        self.gestures.sender().emit(GesturesMsg::LoadProfile(p.input_gestures_active));
        self.numberpad.sender().emit(NumberpadMsg::LoadProfile(p.numberpad_active));
        self.typing.sender().emit(TypingMsg::LoadProfile {
            disable_gestures: p.typing_disable_gestures,
            disable_touchpad: p.typing_disable_touchpad,
            delay_ms: p.typing_reactivation_delay_ms,
        });
        self.fn_key.sender().emit(FnKeyMsg::LoadProfile(p.input_fn_key_locked));

        // System
        self.battery.sender().emit(BatteryMsg::LoadProfile(p.battery_deep_sleep_active));
        self.gpu.sender().emit(GpuMsg::LoadProfile(p.gpu_mode));
        self.apu_mem.sender().emit(ApuMemMsg::LoadProfile(p.apu_mem));
    }
}

#[relm4::component(pub)]
impl SimpleComponent for AppModel {
    type Init = bool;
    type Input = AppMsg;
    type Output = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some(&t!("app_title")),
            set_default_size: (1300, 800),
            set_visible: !model.start_hidden,

            #[wrap(Some)]
            set_content = &model.toast_overlay.clone() -> adw::ToastOverlay {
                #[wrap(Some)]
                set_child = &adw::NavigationSplitView {
                    set_sidebar: Some(&sidebar_nav_page),
                    set_content: Some(&content_nav_page),
                    set_collapsed: false,
                },
            }
        }
    }

    fn update(&mut self, message: AppMsg, sender: ComponentSender<Self>) {
        match message {
            AppMsg::ShowWindow => {
                if let Some(window) = self.window.upgrade() {
                    window.set_visible(true);
                    window.present();
                }
                self.volume_widget.sender().emit(VolumeMsg::Refresh);
                self.refresh_rate.sender().emit(RefreshRateMsg::Refresh);
                self.oled_care.sender().emit(OledCareMsg::Refresh);
                self.battery.sender().emit(BatteryMsg::RefreshStats);
                self.fan.sender().emit(FanMsg::RefreshTelemetry);
                self.armoury.sender().emit(ArmouryMsg::Refresh);
                self.diagnostics.sender().emit(DiagnosticsMsg::Refresh);
            }
            AppMsg::QuitApp => {
                relm4::main_application().quit();
            }
            AppMsg::Error(text) => {
                tracing::warn!("{} {}", t!("error_prefix"), text);
                let toast = adw::Toast::new(&text);
                toast.set_timeout(5);
                self.toast_overlay.add_toast(toast);
            }
            AppMsg::Toast(text) => {
                let toast = adw::Toast::new(&text);
                toast.set_timeout(4);
                self.toast_overlay.add_toast(toast);
            }
            AppMsg::SetLanguage(lang) => {
                AppConfig::update(|c| {
                    c.language = lang.clone();
                });
                rust_i18n::set_locale(&lang);
                let toast = adw::Toast::new(&t!("lang_restart_toast"));
                toast.set_timeout(5);
                self.toast_overlay.add_toast(toast);
            }
            AppMsg::ToggleAutostart(state) => {
                crate::autostart::set_enabled(state);
            }
            AppMsg::ToggleHideUnsupported(active) => {
                if active == self.hide_unsupported_hardware {
                    return;
                }
                self.hide_unsupported_hardware = active;
                AppConfig::update(|c| {
                    c.hide_unsupported_hardware = active;
                });
                self.apply_hardware_visibility();
            }
            AppMsg::TogglePowerAutoSwitch(active) => {
                if self.power_auto_tx.borrow().enabled == active {
                    return;
                }
                AppConfig::update(|c| {
                    c.power_auto_switch_enabled = active;
                });
                let config = AppConfig::load();
                self.power_auto_controls.set_enabled(active);
                self.send_power_auto_settings(&config);
            }
            AppMsg::SetAcProfile(id) => {
                if self.power_auto_tx.borrow().ac_profile_id == id {
                    return;
                }
                AppConfig::update(|c| {
                    c.ac_profile_id = id;
                });
                let config = AppConfig::load();
                self.send_power_auto_settings(&config);
            }
            AppMsg::SetBatteryProfile(id) => {
                if self.power_auto_tx.borrow().battery_profile_id == id {
                    return;
                }
                AppConfig::update(|c| {
                    c.battery_profile_id = id;
                });
                let config = AppConfig::load();
                self.send_power_auto_settings(&config);
            }
            AppMsg::ActivateProfile(id) => {
                let mut config = AppConfig::load();
                if !config.profiles.iter().any(|p| p.id == id) {
                    return;
                }
                if config.active_profile_id != id {
                    config.active_profile_id = id;
                    config.save();
                }
                self.distribute_profile(config.active_profile());
                self.home.sender().emit(HomeMsg::RefreshProfiles);
                self.sync_tray_state();
            }
            AppMsg::ProfilesChanged => {
                let config = AppConfig::load();
                self.power_auto_controls.sync_from_config(&config);
                self.send_power_auto_settings(&config);
                self.sync_tray_state();
            }
            AppMsg::SetFanProfileFromTray(profile) => {
                self.fan.sender().emit(FanMsg::SetFromTray(profile));
            }
            AppMsg::ToggleBatteryMaintenanceFromTray => {
                self.battery
                    .sender()
                    .emit(BatteryMsg::ToggleMaintenanceFromTray);
            }
            AppMsg::FanStateChanged {
                profile,
                asusd_available,
            } => {
                self.tray_fan_profile = profile;
                self.tray_fan_available = asusd_available;
                self.sync_tray_state();
            }
            AppMsg::BatteryStateChanged {
                limit,
                asusd_available,
            } => {
                self.tray_battery_limit = limit;
                self.tray_battery_available = asusd_available;
                self.sync_tray_state();
            }
            AppMsg::HardwareSupportUpdated { slot, supported } => {
                match slot {
                    SupportSlot::Aura => self.hardware_support.aura_supported = Some(supported),
                    SupportSlot::Gpu => self.hardware_support.gpu_supported = Some(supported),
                    SupportSlot::ApuMem => {
                        self.hardware_support.apu_mem_supported = Some(supported)
                    }
                    SupportSlot::Numberpad => {
                        self.hardware_support.numberpad_supported = Some(supported)
                    }
                }
                self.apply_hardware_visibility();
            }
            AppMsg::LegacyMigrationAccepted => {
                match crate::services::migration::perform_migration() {
                    Ok(()) => {}
                    Err(e) => {
                        let toast = adw::Toast::new(&e);
                        toast.set_timeout(5);
                        self.toast_overlay.add_toast(toast);
                    }
                }
            }
            AppMsg::LegacyMigrationDeclined => {
                crate::services::config::AppConfig::update(|c| {
                    c.skip_legacy_migration = true;
                });
            }
            AppMsg::AmbientBacklightChanged => {
                self.backlight_idle.sender().emit(BacklightIdleMsg::AmbientChanged);
            }
            AppMsg::TriggerManualMigration => {
                if !crate::services::migration::legacy_dir_exists() {
                    return;
                }
                let migration_sender = sender.input_sender().clone();
                let window_weak = self.window.clone();
                gtk4::glib::spawn_future_local(async move {
                    let dialog = adw::AlertDialog::builder()
                        .heading(t!("migration_dialog_heading").as_ref())
                        .body(t!("migration_dialog_body").as_ref())
                        .build();
                    dialog.add_response("no", &t!("migration_dialog_no"));
                    dialog.add_response("yes", &t!("migration_dialog_yes"));
                    dialog.set_default_response(Some("yes"));
                    dialog.set_close_response("no");
                    let response = dialog
                        .choose_future(window_weak.upgrade().as_ref())
                        .await;
                    if &*response == "yes" {
                        migration_sender.emit(AppMsg::LegacyMigrationAccepted);
                    } else {
                        migration_sender.emit(AppMsg::LegacyMigrationDeclined);
                    }
                });
            }
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = AppConfig::load();
        let home = HomeModel::builder()
            .launch(!init)
            .forward(sender.input_sender(), |msg| match msg {
                HomeOutput::Error(e) => AppMsg::Error(e),
                HomeOutput::ActivateProfile(id) => AppMsg::ActivateProfile(id),
                HomeOutput::ProfilesChanged => AppMsg::ProfilesChanged,
            });
        let apu_mem = ApuMemModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                ApuMemOutput::Error(e) => AppMsg::Error(e),
                ApuMemOutput::SupportDetected(supported) => AppMsg::HardwareSupportUpdated {
                    slot: SupportSlot::ApuMem,
                    supported,
                },
            });
        let armoury = launch_component!(ArmouryModel, sender);
        let battery = BatteryModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                BatteryOutput::Error(e) => AppMsg::Error(e),
                BatteryOutput::ChargeLimitChanged {
                    limit,
                    asusd_available,
                } => AppMsg::BatteryStateChanged {
                    limit,
                    asusd_available,
                },
            });
        let diagnostics = DiagnosticsModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                DiagnosticsOutput::Toast(text) => AppMsg::Toast(text),
            });
        let fan = FanModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                FanOutput::Error(e) => AppMsg::Error(e),
                FanOutput::StateChanged {
                    profile,
                    asusd_available,
                } => AppMsg::FanStateChanged {
                    profile,
                    asusd_available,
                },
            });
        let gpu = GpuModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                GpuOutput::Error(e) => AppMsg::Error(e),
                GpuOutput::SupportDetected(supported) => AppMsg::HardwareSupportUpdated {
                    slot: SupportSlot::Gpu,
                    supported,
                },
            });
        let refresh_rate = launch_component!(RefreshRateModel, sender);
        let oled_dimming = launch_component!(OledDimmingModel, sender);
        let target_mode = launch_component!(TargetModeModel, sender);
        let oled_care = launch_component!(OledCareModel, sender);
        let color_gamut = launch_component!(ColorGamutModel, sender);
        let aura = AuraPageModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                AuraPageOutput::Error(e) => AppMsg::Error(e),
                AuraPageOutput::SupportDetected(supported) => AppMsg::HardwareSupportUpdated {
                    slot: SupportSlot::Aura,
                    supported,
                },
            });
        let animatrix = launch_component!(AnimatrixModel, sender);
        let fn_key = launch_component!(FnKeyModel, sender);
        let gestures = launch_component!(GesturesModel, sender);
        let numberpad = NumberpadModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                NumberpadOutput::SupportDetected(supported) => AppMsg::HardwareSupportUpdated {
                    slot: SupportSlot::Numberpad,
                    supported,
                },
            });
        let touchpad = launch_component!(TouchpadModel, sender);
        let typing = launch_component!(TypingModel, sender);
        let auto_backlight = AutoBacklightModel::builder()
            .launch(())
            .forward(sender.input_sender(), |msg| match msg {
                AutoBacklightOutput::Error(e) => AppMsg::Error(e),
                AutoBacklightOutput::AmbientChanged => AppMsg::AmbientBacklightChanged,
            });
        let backlight_idle = launch_component!(BacklightIdleModel, sender);
        let sound_modes = launch_component!(SoundModesModel, sender);
        let volume_widget = launch_component!(VolumeModel, sender);

        let initial_fan_profile = FanProfile::from(config.active_profile().fan_profile);
        let initial_tray_state = tray::TrayState {
            profiles: config
                .profiles
                .iter()
                .map(|p| tray::TrayProfileEntry {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    active: p.id == config.active_profile_id,
                })
                .collect(),
            fan_profile: initial_fan_profile,
            fan_available: false,
            battery_limit: 80,
            battery_available: false,
        };
        let tray_svc = ksni::TrayService::new(tray::AyuzTray {
            app_sender: sender.input_sender().clone(),
            state: initial_tray_state,
        });
        let tray_handle = tray_svc.handle();
        tray_svc.spawn();

        let (power_auto_tx, power_auto_rx) =
            tokio::sync::watch::channel(PowerAutoSettings::from(&config));
        tokio::spawn(power_auto::run(sender.input_sender().clone(), power_auto_rx));

        let fan_sender = fan.sender().clone();
        let initial_fan_hotkey_enabled = config.fan_hotkey_enabled;
        let (fan_hotkey_tx, fan_hotkey_rx) =
            tokio::sync::watch::channel(initial_fan_hotkey_enabled);
        tokio::spawn(crate::services::fan_hotkey::run(fan_sender, fan_hotkey_rx));

        // Abstract Unix socket listener for `ayuz --toggle-numberpad`. The
        // CLI short-circuit in main.rs connects to "\0ayuz-numberpad" and
        // writes one byte; each byte received here flips the NumberPad
        // Active/Idle state at runtime without re-launching the GUI.
        let numberpad_sender = numberpad.sender().clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            use tokio::net::UnixListener;
            let addr = match abstract_socket_addr("ayuz-numberpad") {
                Some(a) => a,
                None => return,
            };
            let listener = match std::os::unix::net::UnixListener::bind_addr(&addr)
                .and_then(|s| { s.set_nonblocking(true)?; Ok(s) })
                .and_then(UnixListener::from_std)
            {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("NumberPad IPC: failed to bind socket: {}", e);
                    return;
                }
            };
            loop {
                match listener.accept().await {
                    Ok((mut stream, _)) => {
                        let mut buf = [0u8; 1];
                        if stream.read_exact(&mut buf).await.is_ok() {
                            numberpad_sender.emit(NumberpadMsg::ToggleActive);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("NumberPad IPC: accept failed: {}", e);
                    }
                }
            }
        });

        let toast_overlay = adw::ToastOverlay::new();
        let sidebar_list = gtk4::ListBox::new();
        sidebar_list.add_css_class("navigation-sidebar");
        sidebar_list.set_selection_mode(gtk4::SelectionMode::Single);

        let sorted_nav = Rc::new(sorted_nav_items());
        sidebar_list.set_header_func(|row, before| {
            if before.is_some_and(|b| b.index() == 0) {
                let sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
                sep.set_margin_top(4);
                sep.set_margin_bottom(4);
                sep.add_css_class("nav-separator");
                row.set_header(Some(&sep));
            } else {
                row.set_header(None::<&gtk4::Widget>);
            }
        });

        let mut sidebar_rows = HashMap::new();
        for (icon_name, title_key, page_name) in sorted_nav.iter() {
            let row = gtk4::ListBoxRow::new();
            let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
            hbox.set_margin_top(10);
            hbox.set_margin_bottom(10);
            hbox.set_margin_start(12);
            hbox.set_margin_end(12);
            let icon = gtk4::Image::from_icon_name(icon_name);
            icon.set_pixel_size(16);
            let label = gtk4::Label::new(Some(t!(*title_key).as_ref()));
            label.set_halign(gtk4::Align::Start);
            hbox.append(&icon);
            hbox.append(&label);
            row.set_child(Some(&hbox));
            sidebar_list.append(&row);
            sidebar_rows.insert(*page_name, row);
        }

        let content_stack = adw::ViewStack::new();
        content_stack.set_transition_duration(250);
        content_stack.set_enable_transitions(true);

        let search_visibility = Rc::new(RefCell::new(SearchVisibility::default()));
        let power_auto_controls = PowerAutoControls::new(&sender, &config);
        let hardware_support = HardwareSupportState {
            animatrix_supported: detect_animatrix_hardware() != AnimatrixHardwareType::Unsupported,
            aura_supported: None,
            gpu_supported: None,
            apu_mem_supported: None,
            numberpad_supported: None,
            kde_available: is_kde_desktop(),
            cosmic_available: is_cosmic_desktop(),
            gnome_available: is_gnome_desktop(),
        };

        let model = AppModel {
            start_hidden: init,
            window: root.downgrade(),
            toast_overlay,
            tray: tray_handle,
            tray_fan_profile: initial_fan_profile,
            tray_fan_available: false,
            tray_battery_limit: 80,
            tray_battery_available: false,
            sidebar_list: sidebar_list.clone(),
            sidebar_rows,
            content_stack: content_stack.clone(),
            search_visibility: search_visibility.clone(),
            hide_unsupported_hardware: config.hide_unsupported_hardware,
            hardware_support,
            power_auto_tx,
            power_auto_controls,
            home,
            apu_mem,
            armoury,
            battery,
            diagnostics,
            fan,
            gpu,
            refresh_rate,
            oled_dimming,
            target_mode,
            oled_care,
            color_gamut,
            aura,
            animatrix,
            fn_key,
            gestures,
            numberpad,
            touchpad,
            typing,
            auto_backlight,
            backlight_idle,
            sound_modes,
            volume_widget,
        };

        let home_widget = model.home.widget();
        let apu_mem_widget = model.apu_mem.widget();
        let armoury_widget = model.armoury.widget();
        let battery_widget = model.battery.widget();
        let diagnostics_widget = model.diagnostics.widget();
        let fan_widget = model.fan.widget();
        let gpu_widget = model.gpu.widget();
        let refresh_rate_widget = model.refresh_rate.widget();
        let oled_dimming_widget = model.oled_dimming.widget();
        let target_mode_widget = model.target_mode.widget();
        let oled_care_widget = model.oled_care.widget();
        let color_gamut_widget = model.color_gamut.widget();
        let aura_widget = model.aura.widget();
        let animatrix_widget = model.animatrix.widget();
        let fn_key_widget = model.fn_key.widget();
        let gestures_widget = model.gestures.widget();
        let numberpad_widget = model.numberpad.widget();
        let touchpad_widget = model.touchpad.widget();
        let typing_widget = model.typing.widget();
        let auto_backlight_widget = model.auto_backlight.widget();
        let backlight_idle_widget = model.backlight_idle.widget();
        let sound_modes_widget = model.sound_modes.widget();
        let volume_widget = model.volume_widget.widget();

        // Content pages

        let display_page = adw::PreferencesPage::new();
        display_page.add(refresh_rate_widget);
        display_page.add(oled_dimming_widget);
        display_page.add(target_mode_widget);
        display_page.add(oled_care_widget);
        display_page.add(color_gamut_widget);

        // Aura page hosts a Box of dynamic per-device PreferencesGroups
        // (built by AuraPageModel). PreferencesPage can't host non-PreferencesGroup
        // children directly, so we replicate its clamping/scrolling layout manually.
        let aura_inner = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        aura_inner.append(aura_widget);
        let aura_clamp = adw::Clamp::builder()
            .maximum_size(600)
            .tightening_threshold(400)
            .child(&aura_inner)
            .build();
        let aura_page = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&aura_clamp)
            .build();

        let animatrix_page = adw::PreferencesPage::new();
        animatrix_page.add(animatrix_widget);

        let keyboard_page = adw::PreferencesPage::new();
        keyboard_page.add(auto_backlight_widget);
        keyboard_page.add(backlight_idle_widget);
        keyboard_page.add(fn_key_widget);

        let asus_key_hint_group = build_asus_key_hint_group(fan_hotkey_tx);
        keyboard_page.add(&asus_key_hint_group);

        let touchpad_page = adw::PreferencesPage::new();
        touchpad_page.add(touchpad_widget);
        touchpad_page.add(numberpad_widget);
        touchpad_page.add(gestures_widget);
        touchpad_page.add(typing_widget);

        let audio_page = adw::PreferencesPage::new();
        audio_page.add(volume_widget);
        audio_page.add(sound_modes_widget);

        let system_page = adw::PreferencesPage::new();
        system_page.add(battery_widget);
        system_page.add(fan_widget);
        system_page.add(gpu_widget);
        system_page.add(apu_mem_widget);
        system_page.add(armoury_widget);
        system_page.add(diagnostics_widget);

        let lang_group = build_language_and_autostart_group(&sender);
        system_page.add(&lang_group);
        system_page.add(&model.power_auto_controls.group);

        let legacy_group = build_legacy_migration_group(&sender);
        system_page.add(&legacy_group);

        // Widget map for scroll-to-widget

        let widget_map = std::collections::HashMap::from([
            ("home_info", home_widget.clone().upcast::<gtk4::Widget>()),
            (
                "refresh_rate",
                refresh_rate_widget.clone().upcast::<gtk4::Widget>(),
            ),
            (
                "oled_dimming",
                oled_dimming_widget.clone().upcast::<gtk4::Widget>(),
            ),
            (
                "target_mode",
                target_mode_widget.clone().upcast::<gtk4::Widget>(),
            ),
            (
                "oled_care",
                oled_care_widget.clone().upcast::<gtk4::Widget>(),
            ),
            (
                "color_gamut",
                color_gamut_widget.clone().upcast::<gtk4::Widget>(),
            ),
            ("aura", aura_widget.clone().upcast::<gtk4::Widget>()),
            ("animatrix", animatrix_widget.clone().upcast::<gtk4::Widget>()),
            (
                "auto_backlight",
                auto_backlight_widget.clone().upcast::<gtk4::Widget>(),
            ),
            (
                "backlight_idle",
                backlight_idle_widget.clone().upcast::<gtk4::Widget>(),
            ),
            ("fn_key", fn_key_widget.clone().upcast::<gtk4::Widget>()),
            ("gestures", gestures_widget.clone().upcast::<gtk4::Widget>()),
            ("numberpad", numberpad_widget.clone().upcast::<gtk4::Widget>()),
            ("touchpad", touchpad_widget.clone().upcast::<gtk4::Widget>()),
            ("typing", typing_widget.clone().upcast::<gtk4::Widget>()),
            ("volume", volume_widget.clone().upcast::<gtk4::Widget>()),
            (
                "sound_modes",
                sound_modes_widget.clone().upcast::<gtk4::Widget>(),
            ),
            ("apu_mem", apu_mem_widget.clone().upcast::<gtk4::Widget>()),
            ("armoury", armoury_widget.clone().upcast::<gtk4::Widget>()),
            ("battery", battery_widget.clone().upcast::<gtk4::Widget>()),
            (
                "diagnostics",
                diagnostics_widget.clone().upcast::<gtk4::Widget>(),
            ),
            ("fan", fan_widget.clone().upcast::<gtk4::Widget>()),
            (
                "asus_key_hint",
                asus_key_hint_group.clone().upcast::<gtk4::Widget>(),
            ),
            ("gpu", gpu_widget.clone().upcast::<gtk4::Widget>()),
            ("lang", lang_group.clone().upcast::<gtk4::Widget>()),
            (
                "power_auto",
                model
                    .power_auto_controls
                    .group
                    .clone()
                    .upcast::<gtk4::Widget>(),
            ),
        ]);

        // ViewStack for the content area

        let home_scroll = gtk4::ScrolledWindow::new();
        home_scroll.set_child(Some(home_widget));
        home_scroll.set_vexpand(true);

        content_stack.add_named(&home_scroll, Some(AppPage::Home.as_str()));
        content_stack.add_named(&display_page, Some(AppPage::Display.as_str()));
        content_stack.add_named(&keyboard_page, Some(AppPage::Keyboard.as_str()));
        content_stack.add_named(&aura_page, Some(AppPage::Aura.as_str()));
        content_stack.add_named(&animatrix_page, Some(AppPage::Animatrix.as_str()));
        content_stack.add_named(&touchpad_page, Some(AppPage::Touchpad.as_str()));
        content_stack.add_named(&audio_page, Some(AppPage::Audio.as_str()));
        content_stack.add_named(&system_page, Some(AppPage::System.as_str()));
        content_stack.set_visible_child_name(AppPage::Home.as_str());

        let vol_sender = model.volume_widget.sender().clone();
        let refresh_rate_sender = model.refresh_rate.sender().clone();
        let oled_care_sender = model.oled_care.sender().clone();
        let battery_sender = model.battery.sender().clone();
        let fan_telemetry_sender = model.fan.sender().clone();
        let armoury_sender = model.armoury.sender().clone();
        let diagnostics_sender = model.diagnostics.sender().clone();
        content_stack.connect_notify_local(Some("visible-child-name"), move |stack, _| {
            match stack.visible_child_name().as_deref() {
                Some(name) if name == AppPage::Audio.as_str() => {
                    vol_sender.emit(VolumeMsg::Refresh);
                }
                Some(name) if name == AppPage::Display.as_str() => {
                    refresh_rate_sender.emit(RefreshRateMsg::Refresh);
                    oled_care_sender.emit(OledCareMsg::Refresh);
                }
                Some(name) if name == AppPage::System.as_str() => {
                    battery_sender.emit(BatteryMsg::RefreshStats);
                    fan_telemetry_sender.emit(FanMsg::RefreshTelemetry);
                    armoury_sender.emit(ArmouryMsg::Refresh);
                    diagnostics_sender.emit(DiagnosticsMsg::Refresh);
                }
                _ => {}
            }
        });

        let content_header = adw::HeaderBar::new();
        let content_toolbar = adw::ToolbarView::new();
        content_toolbar.add_top_bar(&content_header);
        content_toolbar.set_content(Some(&content_stack));
        let content_nav_page = adw::NavigationPage::new(&content_toolbar, &t!("tab_display"));

        // Sidebar wiring

        let stack_c = content_stack.clone();
        let nav_page_c = content_nav_page.clone();
        let sorted_nav_c = sorted_nav.clone();
        sidebar_list.connect_row_selected(move |_, row| {
            if let Some(row) = row {
                let idx = row.index() as usize;
                if let Some(&(_, title_key, page_name)) = sorted_nav_c.get(idx) {
                    stack_c.set_visible_child_name(page_name);
                    nav_page_c.set_title(&t!(title_key));
                }
            }
        });

        if let Some(first_row) = sidebar_list.row_at_index(0) {
            sidebar_list.select_row(Some(&first_row));
        }

        // Search

        let search_widgets = crate::search::setup(
            (*sorted_nav).clone(),
            &content_stack,
            &content_nav_page,
            &sidebar_list,
            widget_map,
            search_visibility,
        );
        content_stack.add_named(&search_widgets.scroll, Some(AppPage::Search.as_str()));

        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.pack_end(&search_widgets.toggle);

        {
            let title_label = gtk4::Label::new(Some(&t!("app_title")));
            title_label.add_css_class("title");
            sidebar_header.set_title_widget(Some(&title_label));
        }

        let sidebar_toolbar = adw::ToolbarView::new();
        sidebar_toolbar.add_top_bar(&sidebar_header);
        sidebar_toolbar.add_top_bar(&search_widgets.bar);
        sidebar_toolbar.set_content(Some(&sidebar_list));

        // Bottom bar: GitHub + "Made by Guido" + version
        {
            let bottom_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
            bottom_box.set_margin_top(6);
            bottom_box.set_margin_bottom(6);
            bottom_box.set_margin_start(10);
            bottom_box.set_margin_end(10);

            let github_btn = gtk4::Button::new();
            github_btn.add_css_class("flat");
            github_btn.set_tooltip_text(Some("GitHub"));
            let svg_bytes = include_bytes!("../assets/img/github.svg");
            let glib_bytes = gtk4::glib::Bytes::from_static(svg_bytes);
            if let Ok(texture) = gtk4::gdk::Texture::from_bytes(&glib_bytes) {
                let gh_icon = gtk4::Image::from_paintable(Some(&texture));
                gh_icon.set_pixel_size(16);
                github_btn.set_child(Some(&gh_icon));
            }
            github_btn.connect_clicked(|_| {
                let _ = Command::new("xdg-open")
                    .arg("https://github.com/Traciges")
                    .process_group(0)
                    .spawn();
            });

            let made_by_label = gtk4::Label::new(Some("Made by Guido"));
            made_by_label.add_css_class("dim-label");
            made_by_label.set_margin_start(6);
            made_by_label.set_valign(gtk4::Align::Center);

            let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
            spacer.set_hexpand(true);

            let version_label = gtk4::Label::new(Some(concat!("v", env!("CARGO_PKG_VERSION"))));
            version_label.add_css_class("dim-label");
            version_label.set_valign(gtk4::Align::Center);

            bottom_box.append(&github_btn);
            bottom_box.append(&made_by_label);
            bottom_box.append(&spacer);
            bottom_box.append(&version_label);

            sidebar_toolbar.add_bottom_bar(&bottom_box);
        }

        let sidebar_nav_page = adw::NavigationPage::new(&sidebar_toolbar, &t!("app_title"));

        // Build widget tree

        let widgets = view_output!();
        model.apply_hardware_visibility();

        root.set_hide_on_close(true);

        // Pause Home metrics polling while the window is hidden (`--hidden` or closed to tray)
        // or while another tab is active, and resume immediately when Home becomes visible.
        // `suppress_initial_present` prevents Relm4's initial `present()` call before
        // `idle_add_local_once` hides a `--hidden` window from triggering a spurious cycle.
        let suppress_initial_present = Rc::new(std::cell::Cell::new(init));
        let update_home_polling: Rc<dyn Fn()> = {
            let home_sender = model.home.sender().clone();
            let win_weak = root.downgrade();
            let stack_weak = content_stack.downgrade();
            let suppress = suppress_initial_present.clone();
            Rc::new(move || {
                if suppress.get() {
                    return;
                }
                let active = win_weak
                    .upgrade()
                    .is_some_and(|w| w.is_visible())
                    && stack_weak.upgrade().is_some_and(|s| {
                        s.visible_child_name().as_deref() == Some(AppPage::Home.as_str())
                    });
                home_sender.emit(HomeMsg::SetPollingActive(active));
            })
        };
        {
            let update = update_home_polling.clone();
            root.connect_visible_notify(move |_| update());
        }
        {
            let update = update_home_polling;
            content_stack.connect_notify_local(Some("visible-child-name"), move |_, _| update());
        }

        // Relm4 calls window.present() internally after init() completes, which
        // overrides set_visible: false and forces the window to show. We schedule
        // a hide in the same GTK frame via idle_add_local_once so the window is
        // invisible again before a single pixel is drawn to the screen.
        if init {
            let win = root.clone();
            let suppress = suppress_initial_present;
            gtk4::glib::idle_add_local_once(move || {
                win.set_visible(false);
                suppress.set(false);
            });
        }

        if crate::services::migration::should_prompt() {
            let migration_sender = sender.input_sender().clone();
            let window_weak = root.downgrade();
            gtk4::glib::spawn_future_local(async move {
                let dialog = adw::AlertDialog::builder()
                    .heading(t!("migration_dialog_heading").as_ref())
                    .body(t!("migration_dialog_body").as_ref())
                    .build();
                dialog.add_response("no", &t!("migration_dialog_no"));
                dialog.add_response("yes", &t!("migration_dialog_yes"));
                dialog.set_default_response(Some("yes"));
                dialog.set_close_response("no");
                let response = dialog
                    .choose_future(window_weak.upgrade().as_ref())
                    .await;
                if &*response == "yes" {
                    migration_sender.emit(AppMsg::LegacyMigrationAccepted);
                } else {
                    migration_sender.emit(AppMsg::LegacyMigrationDeclined);
                }
            });
        }

        ComponentParts { model, widgets }
    }
}

// ── PreferencesGroup builders ────────────────────────────────────────────────
//
// Each builder constructs one `adw::PreferencesGroup` together with the rows
// inside it and wires up the relevant signals. They are called from `init()`
// so that the top-level component code stays focused on page composition.

/// Hint group hosted on the keyboard page. Surfaces the ASUS hardware-key
/// shortcut help row plus two app-level toggles (fan-profile hotkey + OSD).
///
/// `fan_hotkey_tx` is the `watch::Sender` that `services::fan_hotkey::run`
/// subscribes to — toggling the row pushes the new state to the running task.
fn build_asus_key_hint_group(
    fan_hotkey_tx: tokio::sync::watch::Sender<bool>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&t!("asus_key_hint_group_title"));

    let hint_row = adw::ActionRow::new();
    hint_row.set_title(&t!("asus_key_hint_row_title"));
    hint_row.set_subtitle(&t!("asus_key_hint_row_subtitle"));
    group.add(&hint_row);

    let cfg = crate::services::config::AppConfig::load();

    let fan_hotkey_row = adw::SwitchRow::new();
    fan_hotkey_row.set_title(&t!("fan_hotkey_enable_title"));
    fan_hotkey_row.set_subtitle(&t!("fan_hotkey_enable_subtitle"));
    fan_hotkey_row.set_active(cfg.fan_hotkey_enabled);
    fan_hotkey_row.connect_active_notify(move |row| {
        let active = row.is_active();
        crate::services::config::AppConfig::update(|c| c.fan_hotkey_enabled = active);
        let _ = fan_hotkey_tx.send(active);
    });
    group.add(&fan_hotkey_row);

    let fan_osd_row = adw::SwitchRow::new();
    fan_osd_row.set_title(&t!("fan_osd_enabled_title"));
    fan_osd_row.set_subtitle(&t!("fan_osd_enabled_subtitle"));
    fan_osd_row.set_active(cfg.fan_osd_enabled);
    fan_osd_row.connect_active_notify(|row| {
        let active = row.is_active();
        crate::services::config::AppConfig::update(|c| c.fan_osd_enabled = active);
    });
    group.add(&fan_osd_row);

    group
}

/// "App settings" group on the system page: language dropdown + autostart switch.
fn build_language_and_autostart_group(
    sender: &ComponentSender<AppModel>,
) -> adw::PreferencesGroup {
    const SUPPORTED_LANGS: &[(&str, &str)] = &[
        ("English", "en"),
        ("Deutsch", "de"),
        ("Português Brasileiro", "pt-br"),
    ];

    let group = adw::PreferencesGroup::new();
    group.set_title(&t!("app_settings_title"));

    let lang_row = adw::ActionRow::new();
    lang_row.set_title(&t!("language_title"));

    let display_names: Vec<&str> = SUPPORTED_LANGS.iter().map(|(name, _)| *name).collect();
    let lang_dropdown = gtk4::DropDown::from_strings(&display_names);
    lang_dropdown.set_valign(gtk4::Align::Center);

    let cfg = AppConfig::load();
    let current_lang = cfg.language;
    if let Some(idx) = SUPPORTED_LANGS
        .iter()
        .position(|(_, code)| *code == current_lang)
    {
        lang_dropdown.set_selected(idx as u32);
    }

    let sender_lang = sender.clone();
    lang_dropdown.connect_selected_notify(move |dd| {
        let idx = dd.selected() as usize;
        if let Some(&(_, code)) = SUPPORTED_LANGS.get(idx) {
            sender_lang.input(AppMsg::SetLanguage(code.to_string()));
        }
    });

    lang_row.add_suffix(&lang_dropdown);
    lang_row.set_activatable_widget(Some(&lang_dropdown));
    group.add(&lang_row);

    let autostart_row = adw::SwitchRow::new();
    autostart_row.set_title(&t!("autostart_title"));
    autostart_row.set_active(crate::autostart::is_enabled());
    let sender_at = sender.clone();
    autostart_row.connect_active_notify(move |row| {
        sender_at.input(AppMsg::ToggleAutostart(row.is_active()));
    });
    group.add(&autostart_row);

    let hide_unsupported_row = adw::SwitchRow::new();
    hide_unsupported_row.set_title(&t!("hide_unsupported_title"));
    hide_unsupported_row.set_subtitle(&t!("hide_unsupported_subtitle"));
    hide_unsupported_row.set_active(cfg.hide_unsupported_hardware);
    let sender_hide = sender.clone();
    hide_unsupported_row.connect_active_notify(move |row| {
        sender_hide.input(AppMsg::ToggleHideUnsupported(row.is_active()));
    });
    group.add(&hide_unsupported_row);

    group
}

/// One-shot migration helper shown on the system page when an
/// `ayuz-old` config directory exists from a pre-1.0 install.
fn build_legacy_migration_group(sender: &ComponentSender<AppModel>) -> adw::PreferencesGroup {
    let available = crate::services::migration::legacy_dir_exists();

    let group = adw::PreferencesGroup::new();
    group.set_title(&t!("legacy_migration_group_title"));
    group.set_description(Some(&t!("legacy_migration_group_desc")));

    let row = adw::ActionRow::new();
    row.set_title(&t!("legacy_migration_row_title"));
    row.set_subtitle(&t!(if available {
        "legacy_migration_row_subtitle_available"
    } else {
        "legacy_migration_row_subtitle_unavailable"
    }));

    let btn = gtk4::Button::with_label(&t!("legacy_migration_button"));
    btn.add_css_class("suggested-action");
    btn.set_valign(gtk4::Align::Center);
    btn.set_sensitive(available);

    let sender_clone = sender.clone();
    btn.connect_clicked(move |_| {
        sender_clone.input(AppMsg::TriggerManualMigration);
    });

    row.add_suffix(&btn);
    group.add(&row);
    group
}
