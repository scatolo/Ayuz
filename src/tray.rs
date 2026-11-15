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

use crate::app::AppMsg;
use crate::services::dbus::FanProfile;
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Icon, MenuItem, Tray};
use relm4::Sender;
use rust_i18n::t;

/// Profile item rendered in the tray's Profiles submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayProfileEntry {
    pub id: String,
    pub name: String,
    pub active: bool,
}

/// Live state synchronized into the system tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    pub profiles: Vec<TrayProfileEntry>,
    pub fan_profile: FanProfile,
    pub fan_available: bool,
    pub battery_limit: u8,
    pub battery_available: bool,
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            fan_profile: FanProfile::Balanced,
            fan_available: false,
            battery_limit: 80,
            battery_available: false,
        }
    }
}

pub struct AyuzTray {
    pub app_sender: Sender<AppMsg>,
    pub state: TrayState,
}

impl Tray for AyuzTray {
    fn id(&self) -> String {
        "Ayuz".into()
    }

    fn title(&self) -> String {
        "Ayuz".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.app_sender.emit(AppMsg::ShowWindow);
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        let icon_bytes = include_bytes!("../assets/trayicon.png");
        let Ok(img) = image::load_from_memory(icon_bytes) else {
            return Vec::new();
        };
        let rgba = img.into_rgba8();
        let (width, height) = rgba.dimensions();
        let mut argb = rgba.into_raw();
        for px in argb.chunks_exact_mut(4) {
            px.rotate_right(1);
        }
        vec![Icon {
            width: width as i32,
            height: height as i32,
            data: argb,
        }]
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let sender_show = self.app_sender.clone();
        let sender_quit = self.app_sender.clone();

        let profile_items: Vec<MenuItem<Self>> = self
            .state
            .profiles
            .iter()
            .map(|entry| {
                let sender = self.app_sender.clone();
                let id = entry.id.clone();
                MenuItem::Checkmark(CheckmarkItem {
                    label: entry.name.clone(),
                    checked: entry.active,
                    activate: Box::new(move |_| {
                        sender.emit(AppMsg::ActivateProfile(id.clone()));
                    }),
                    ..Default::default()
                })
            })
            .collect();

        let fan_modes = [
            (
                t!("fan_performance_title").to_string(),
                FanProfile::Performance,
                self.state.fan_profile == FanProfile::Performance,
            ),
            (
                t!("fan_balanced_title").to_string(),
                FanProfile::Balanced,
                self.state.fan_profile == FanProfile::Balanced,
            ),
            (
                t!("fan_quiet_title").to_string(),
                FanProfile::Quiet,
                matches!(
                    self.state.fan_profile,
                    FanProfile::Quiet | FanProfile::LowPower
                ),
            ),
        ];

        let fan_items: Vec<MenuItem<Self>> = fan_modes
            .into_iter()
            .map(|(label, profile, checked)| {
                let sender = self.app_sender.clone();
                MenuItem::Checkmark(CheckmarkItem {
                    label,
                    checked,
                    enabled: self.state.fan_available,
                    activate: Box::new(move |_| {
                        sender.emit(AppMsg::SetFanProfileFromTray(profile));
                    }),
                    ..Default::default()
                })
            })
            .collect();

        let sender_battery = self.app_sender.clone();

        vec![
            MenuItem::Standard(StandardItem {
                label: t!("tray_show").to_string(),
                activate: Box::new(move |_| {
                    sender_show.emit(AppMsg::ShowWindow);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::SubMenu(SubMenu {
                label: t!("tray_profiles").to_string(),
                enabled: !profile_items.is_empty(),
                submenu: profile_items,
                ..Default::default()
            }),
            MenuItem::SubMenu(SubMenu {
                label: t!("tray_fan_mode").to_string(),
                submenu: fan_items,
                ..Default::default()
            }),
            MenuItem::Checkmark(CheckmarkItem {
                label: t!("tray_battery_maintenance").to_string(),
                checked: self.state.battery_limit <= 80,
                enabled: self.state.battery_available,
                activate: Box::new(move |_| {
                    sender_battery.emit(AppMsg::ToggleBatteryMaintenanceFromTray);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: t!("tray_exit").to_string(),
                activate: Box::new(move |_| {
                    sender_quit.emit(AppMsg::QuitApp);
                }),
                ..Default::default()
            }),
        ]
    }
}
