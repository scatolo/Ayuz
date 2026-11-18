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

use std::cell::Cell;
use std::rc::Rc;

use gtk4 as gtk;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;

use super::cosmic_panel;
use super::helpers::run_qdbus;
use crate::services::commands::{
    is_cosmic_desktop, is_gnome_desktop, is_kde_desktop, run_command_blocking,
};
use crate::services::config::AppConfig;

const GNOME_IDLE_DELAY_PRESETS: &[Option<u32>] = &[
    None,
    Some(60),
    Some(120),
    Some(180),
    Some(300),
    Some(600),
    Some(0),
];

/// Parses `gsettings get org.gnome.desktop.session idle-delay` output (e.g. `"uint32 180"` or `"180"`).
pub(crate) fn parse_gsettings_uint32(raw: &str) -> Option<u32> {
    raw.split_whitespace().last()?.parse::<u32>().ok()
}

/// Parses `gsettings get ...` boolean output (`"true"` / `"false"`).
pub(crate) fn parse_gsettings_bool(raw: &str) -> Option<bool> {
    match raw.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn format_idle_delay_value(secs: u32) -> String {
    match secs {
        0 => t!("oled_care_idle_never").to_string(),
        60 => t!("sleep_timeout_1min").to_string(),
        120 => t!("sleep_timeout_2min").to_string(),
        300 => t!("sleep_timeout_5min").to_string(),
        s if s % 60 == 0 => t!("oled_care_idle_minutes", mins = (s / 60).to_string()).to_string(),
        s => t!("oled_care_idle_seconds", secs = s.to_string()).to_string(),
    }
}

pub struct OledCareModel {
    pixel_refresh_active: bool,
    panel_autohide_active: bool,
    transparency_active: bool,
    gnome_screen_blank_secs: Option<u32>,
    gnome_system_idle_secs: Option<u32>,
    gnome_ambient_active: bool,
    gnome_ambient_configured: Option<bool>,
    gnome_idle_dim_active: bool,
    gnome_idle_dim_configured: Option<bool>,
    kde_available: bool,
    panel_ctrl_available: bool,
    gnome_available: bool,
    hide_unsupported: bool,
    screen_blank_combo: adw::ComboRow,
    ambient_switch: adw::SwitchRow,
    idle_dim_switch: adw::SwitchRow,
    suppress_signals: Rc<Cell<bool>>,
}

#[derive(Debug)]
pub enum OledCareMsg {
    TogglePixelRefresh(bool),
    TogglePanelAutohide(bool),
    ToggleTransparency(bool),
    SelectScreenBlank(u32),
    ToggleGnomeAmbient(bool),
    ToggleGnomeIdleDim(bool),
    SetHideUnsupported(bool),
    Refresh,
    LoadProfile {
        pixel_refresh: bool,
        panel_autohide: bool,
        transparency: bool,
        gnome_screen_blank_secs: Option<u32>,
        gnome_ambient_brightness: Option<bool>,
        gnome_idle_dim: Option<bool>,
    },
}

#[derive(Debug)]
pub enum OledCareCommandOutput {
    PanelSet(bool),
    TransparencySet(bool),
    PixelRefreshSet(bool),
    GnomeStateLoaded {
        idle_delay: Option<u32>,
        ambient_enabled: Option<bool>,
        idle_dim: Option<bool>,
    },
    GnomeScreenBlankSet(u32),
    GnomeScreenBlankFailed {
        error: String,
        rollback_secs: Option<u32>,
    },
    GnomeAmbientSet(bool),
    GnomeAmbientFailed {
        error: String,
        rollback_active: bool,
        rollback_configured: Option<bool>,
    },
    GnomeIdleDimSet(bool),
    GnomeIdleDimFailed {
        error: String,
        rollback_active: bool,
        rollback_configured: Option<bool>,
    },
    Error(String),
}

impl OledCareModel {
    fn rebuild_screen_blank_combo(&self) {
        let keep_label = match self.gnome_system_idle_secs {
            Some(secs) => t!(
                "oled_care_idle_keep_current_active",
                val = format_idle_delay_value(secs)
            )
            .to_string(),
            None => t!("oled_care_idle_keep_current").to_string(),
        };

        let mut labels: Vec<String> = Vec::with_capacity(GNOME_IDLE_DELAY_PRESETS.len());
        labels.push(keep_label);
        for preset in &GNOME_IDLE_DELAY_PRESETS[1..] {
            if let Some(secs) = preset {
                labels.push(format_idle_delay_value(*secs));
            }
        }

        let str_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected_idx = GNOME_IDLE_DELAY_PRESETS
            .iter()
            .position(|&p| p == self.gnome_screen_blank_secs)
            .unwrap_or(0) as u32;

        self.suppress_signals.set(true);
        self.screen_blank_combo
            .set_model(Some(&gtk::StringList::new(&str_refs)));
        self.screen_blank_combo.set_selected(selected_idx);
        self.suppress_signals.set(false);
    }

    fn sync_gnome_switches(&self) {
        self.suppress_signals.set(true);
        self.ambient_switch.set_active(self.gnome_ambient_active);
        self.idle_dim_switch.set_active(self.gnome_idle_dim_active);
        self.suppress_signals.set(false);
    }
}

#[relm4::component(pub)]
impl Component for OledCareModel {
    type Init = ();
    type Input = OledCareMsg;
    type Output = String;
    type CommandOutput = OledCareCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &t!("oled_care_group_title"),
            set_description: Some(&t!("oled_care_group_desc")),

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                #[watch]
                set_visible: !(model.panel_ctrl_available || model.gnome_available),
                set_label: &t!("oled_care_desktop_required"),
            },

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                set_label: &t!("oled_care_group_notice"),
                #[watch]
                set_visible: !model.hide_unsupported || (!model.gnome_available && !model.panel_ctrl_available),
            },

            add = &model.screen_blank_combo.clone() -> adw::ComboRow {
                set_title: &t!("oled_care_screen_blank_title"),
                set_subtitle: &t!("oled_care_screen_blank_subtitle"),
                #[watch]
                set_sensitive: model.gnome_available,
                #[watch]
                set_visible: !model.hide_unsupported || model.gnome_available,
            },

            add = &model.ambient_switch.clone() -> adw::SwitchRow {
                set_title: &t!("oled_care_ambient_brightness_title"),
                set_subtitle: &t!("oled_care_ambient_brightness_subtitle"),
                #[watch]
                set_sensitive: model.gnome_available,
                #[watch]
                set_visible: !model.hide_unsupported || model.gnome_available,
            },

            add = &model.idle_dim_switch.clone() -> adw::SwitchRow {
                set_title: &t!("oled_care_idle_dim_title"),
                set_subtitle: &t!("oled_care_idle_dim_subtitle"),
                #[watch]
                set_sensitive: model.gnome_available,
                #[watch]
                set_visible: !model.hide_unsupported || model.gnome_available,
            },

            add = &adw::SwitchRow {
                set_title: &t!("oled_care_pixel_refresh_title"),
                set_subtitle: &t!("oled_care_pixel_refresh_subtitle"),

                #[watch]
                set_active: model.pixel_refresh_active,
                #[watch]
                set_sensitive: model.kde_available,
                #[watch]
                set_visible: !model.hide_unsupported || model.kde_available,

                connect_active_notify[sender] => move |switch| {
                    sender.input(OledCareMsg::TogglePixelRefresh(switch.is_active()));
                },
            },

            add = &adw::SwitchRow {
                set_title: &t!("oled_care_panel_autohide_title"),
                set_subtitle: &t!("oled_care_panel_autohide_subtitle"),

                #[watch]
                set_active: model.panel_autohide_active,
                #[watch]
                set_sensitive: model.panel_ctrl_available,
                #[watch]
                set_visible: !model.hide_unsupported || model.panel_ctrl_available,

                connect_active_notify[sender] => move |switch| {
                    sender.input(OledCareMsg::TogglePanelAutohide(switch.is_active()));
                },
            },

            add = &adw::SwitchRow {
                set_title: &t!("oled_care_transparency_title"),
                set_subtitle: &t!("oled_care_transparency_subtitle"),

                #[watch]
                set_active: model.transparency_active,
                #[watch]
                set_sensitive: model.panel_ctrl_available,
                #[watch]
                set_visible: !model.hide_unsupported || model.panel_ctrl_available,

                connect_active_notify[sender] => move |switch| {
                    sender.input(OledCareMsg::ToggleTransparency(switch.is_active()));
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
        let p = config.active_profile();
        let kde = is_kde_desktop();
        let cosmic = is_cosmic_desktop();
        let gnome = is_gnome_desktop();

        let screen_blank_combo = adw::ComboRow::new();
        let ambient_switch = adw::SwitchRow::new();
        let idle_dim_switch = adw::SwitchRow::new();
        let suppress_signals = Rc::new(Cell::new(false));
        {
            let sender = sender.clone();
            let suppress = suppress_signals.clone();
            screen_blank_combo.connect_selected_notify(move |row| {
                if suppress.get() {
                    return;
                }
                sender.input(OledCareMsg::SelectScreenBlank(row.selected()));
            });
        }
        {
            let sender = sender.clone();
            let suppress = suppress_signals.clone();
            ambient_switch.connect_active_notify(move |switch| {
                if suppress.get() {
                    return;
                }
                sender.input(OledCareMsg::ToggleGnomeAmbient(switch.is_active()));
            });
        }
        {
            let sender = sender.clone();
            let suppress = suppress_signals.clone();
            idle_dim_switch.connect_active_notify(move |switch| {
                if suppress.get() {
                    return;
                }
                sender.input(OledCareMsg::ToggleGnomeIdleDim(switch.is_active()));
            });
        }

        let model = OledCareModel {
            pixel_refresh_active: p.oled_care_pixel_refresh,
            panel_autohide_active: p.oled_care_panel_autohide,
            transparency_active: p.oled_care_transparency,
            gnome_screen_blank_secs: p.gnome_screen_blank_secs,
            gnome_system_idle_secs: None,
            gnome_ambient_active: p.gnome_ambient_brightness.unwrap_or(false),
            gnome_ambient_configured: p.gnome_ambient_brightness,
            gnome_idle_dim_active: p.gnome_idle_dim.unwrap_or(false),
            gnome_idle_dim_configured: p.gnome_idle_dim,
            kde_available: kde,
            panel_ctrl_available: kde || cosmic,
            gnome_available: gnome,
            hide_unsupported: config.hide_unsupported_hardware,
            screen_blank_combo,
            ambient_switch,
            idle_dim_switch,
            suppress_signals,
        };
        model.rebuild_screen_blank_combo();
        model.sync_gnome_switches();

        let widgets = view_output!();

        if gnome {
            let configured_blank = model.gnome_screen_blank_secs;
            let configured_ambient = model.gnome_ambient_configured;
            let configured_idle_dim = model.gnome_idle_dim_configured;
            sender.command(move |out, shutdown| {
                shutdown
                    .register(async move {
                        if let Some(secs) = configured_blank {
                            let _ = set_gnome_idle_delay(secs).await;
                        }
                        if let Some(enabled) = configured_ambient {
                            let _ = set_gnome_ambient_enabled(enabled).await;
                        }
                        if let Some(enabled) = configured_idle_dim {
                            let _ = set_gnome_idle_dim(enabled).await;
                        }
                        let state = read_gnome_oled_care_state().await;
                        out.emit(OledCareCommandOutput::GnomeStateLoaded {
                            idle_delay: state.idle_delay,
                            ambient_enabled: state.ambient_enabled,
                            idle_dim: state.idle_dim,
                        });
                    })
                    .drop_on_shutdown()
            });
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: OledCareMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            OledCareMsg::TogglePixelRefresh(active) => {
                if active == self.pixel_refresh_active {
                    return;
                }
                self.pixel_refresh_active = active;

                AppConfig::update(|c| c.active_profile_mut().oled_care_pixel_refresh = active);

                let idle_time = if active { "300" } else { "600" };
                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match run_command_blocking(
                                "kwriteconfig6",
                                &[
                                    "--file",
                                    "powermanagementprofilesrc",
                                    "--group",
                                    "AC",
                                    "--group",
                                    "DPMSControl",
                                    "--key",
                                    "idleTime",
                                    idle_time,
                                ],
                            )
                            .await
                            {
                                Ok(()) => out.emit(OledCareCommandOutput::PixelRefreshSet(active)),
                                Err(e) => out.emit(OledCareCommandOutput::Error(e)),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
            OledCareMsg::TogglePanelAutohide(active) => {
                if active == self.panel_autohide_active {
                    return;
                }
                self.panel_autohide_active = active;

                AppConfig::update(|c| c.active_profile_mut().oled_care_panel_autohide = active);

                if is_kde_desktop() {
                    let hiding = if active { "autohide" } else { "none" };
                    let script =
                        format!("panels().forEach(function(p){{p.hiding='{}';}})", hiding);
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                plasmashell_evaluate(
                                    &script,
                                    &out,
                                    OledCareCommandOutput::PanelSet(active),
                                )
                                .await;
                            })
                            .drop_on_shutdown()
                    });
                } else if is_cosmic_desktop() {
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match cosmic_panel::set_autohide_all(active).await {
                                    Ok(()) => out.emit(OledCareCommandOutput::PanelSet(active)),
                                    Err(e) => out.emit(OledCareCommandOutput::Error(e)),
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            OledCareMsg::ToggleTransparency(active) => {
                if active == self.transparency_active {
                    return;
                }
                self.transparency_active = active;

                AppConfig::update(|c| c.active_profile_mut().oled_care_transparency = active);

                if is_kde_desktop() {
                    let opacity = if active { "transparent" } else { "opaque" };
                    let script =
                        format!("panels().forEach(function(p){{p.opacity='{}';}})", opacity);
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                plasmashell_evaluate(
                                    &script,
                                    &out,
                                    OledCareCommandOutput::TransparencySet(active),
                                )
                                .await;
                            })
                            .drop_on_shutdown()
                    });
                } else if is_cosmic_desktop() {
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match cosmic_panel::set_opacity_all(active).await {
                                    Ok(()) => {
                                        out.emit(OledCareCommandOutput::TransparencySet(active))
                                    }
                                    Err(e) => out.emit(OledCareCommandOutput::Error(e)),
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            OledCareMsg::SelectScreenBlank(idx) => {
                let Some(&new_val) = GNOME_IDLE_DELAY_PRESETS.get(idx as usize) else {
                    return;
                };
                if new_val == self.gnome_screen_blank_secs {
                    return;
                }
                let prev_val = self.gnome_screen_blank_secs;
                self.gnome_screen_blank_secs = new_val;
                AppConfig::update(|c| c.active_profile_mut().gnome_screen_blank_secs = new_val);

                if let Some(secs) = new_val
                    && self.gnome_available
                {
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match set_gnome_idle_delay(secs).await {
                                    Ok(()) => {
                                        out.emit(OledCareCommandOutput::GnomeScreenBlankSet(secs))
                                    }
                                    Err(error) => {
                                        out.emit(OledCareCommandOutput::GnomeScreenBlankFailed {
                                            error,
                                            rollback_secs: prev_val,
                                        })
                                    }
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            OledCareMsg::ToggleGnomeAmbient(active) => {
                if active == self.gnome_ambient_active {
                    return;
                }
                let prev_active = self.gnome_ambient_active;
                let prev_configured = self.gnome_ambient_configured;
                self.gnome_ambient_active = active;
                self.gnome_ambient_configured = Some(active);
                AppConfig::update(|c| {
                    c.active_profile_mut().gnome_ambient_brightness = Some(active)
                });

                if self.gnome_available {
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match set_gnome_ambient_enabled(active).await {
                                    Ok(()) => {
                                        out.emit(OledCareCommandOutput::GnomeAmbientSet(active))
                                    }
                                    Err(error) => {
                                        out.emit(OledCareCommandOutput::GnomeAmbientFailed {
                                            error,
                                            rollback_active: prev_active,
                                            rollback_configured: prev_configured,
                                        })
                                    }
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            OledCareMsg::ToggleGnomeIdleDim(active) => {
                if active == self.gnome_idle_dim_active {
                    return;
                }
                let prev_active = self.gnome_idle_dim_active;
                let prev_configured = self.gnome_idle_dim_configured;
                self.gnome_idle_dim_active = active;
                self.gnome_idle_dim_configured = Some(active);
                AppConfig::update(|c| c.active_profile_mut().gnome_idle_dim = Some(active));

                if self.gnome_available {
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match set_gnome_idle_dim(active).await {
                                    Ok(()) => {
                                        out.emit(OledCareCommandOutput::GnomeIdleDimSet(active))
                                    }
                                    Err(error) => {
                                        out.emit(OledCareCommandOutput::GnomeIdleDimFailed {
                                            error,
                                            rollback_active: prev_active,
                                            rollback_configured: prev_configured,
                                        })
                                    }
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            OledCareMsg::Refresh => {
                if !self.gnome_available {
                    return;
                }
                sender.command(|out, shutdown| {
                    shutdown
                        .register(async move {
                            let state = read_gnome_oled_care_state().await;
                            out.emit(OledCareCommandOutput::GnomeStateLoaded {
                                idle_delay: state.idle_delay,
                                ambient_enabled: state.ambient_enabled,
                                idle_dim: state.idle_dim,
                            });
                        })
                        .drop_on_shutdown()
                });
            }
            OledCareMsg::LoadProfile {
                pixel_refresh,
                panel_autohide,
                transparency,
                gnome_screen_blank_secs,
                gnome_ambient_brightness,
                gnome_idle_dim,
            } => {
                if self.panel_ctrl_available {
                    if self.kde_available {
                        sender.input(OledCareMsg::TogglePixelRefresh(pixel_refresh));
                    }
                    sender.input(OledCareMsg::TogglePanelAutohide(panel_autohide));
                    sender.input(OledCareMsg::ToggleTransparency(transparency));
                }

                if self.gnome_available {
                    self.gnome_screen_blank_secs = gnome_screen_blank_secs;
                    self.gnome_ambient_configured = gnome_ambient_brightness;
                    self.gnome_idle_dim_configured = gnome_idle_dim;
                    self.rebuild_screen_blank_combo();

                    if let Some(enabled) = gnome_ambient_brightness {
                        self.gnome_ambient_active = enabled;
                    }
                    if let Some(enabled) = gnome_idle_dim {
                        self.gnome_idle_dim_active = enabled;
                    }
                    self.sync_gnome_switches();

                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                if let Some(secs) = gnome_screen_blank_secs {
                                    match set_gnome_idle_delay(secs).await {
                                        Ok(()) => out.emit(
                                            OledCareCommandOutput::GnomeScreenBlankSet(secs),
                                        ),
                                        Err(e) => out.emit(OledCareCommandOutput::Error(e)),
                                    }
                                }
                                if let Some(enabled) = gnome_ambient_brightness {
                                    match set_gnome_ambient_enabled(enabled).await {
                                        Ok(()) => out.emit(
                                            OledCareCommandOutput::GnomeAmbientSet(enabled),
                                        ),
                                        Err(e) => out.emit(OledCareCommandOutput::Error(e)),
                                    }
                                }
                                if let Some(enabled) = gnome_idle_dim {
                                    match set_gnome_idle_dim(enabled).await {
                                        Ok(()) => out.emit(
                                            OledCareCommandOutput::GnomeIdleDimSet(enabled),
                                        ),
                                        Err(e) => out.emit(OledCareCommandOutput::Error(e)),
                                    }
                                }
                                if gnome_ambient_brightness.is_none()
                                    || gnome_idle_dim.is_none()
                                    || gnome_screen_blank_secs.is_none()
                                {
                                    let state = read_gnome_oled_care_state().await;
                                    out.emit(OledCareCommandOutput::GnomeStateLoaded {
                                        idle_delay: state.idle_delay,
                                        ambient_enabled: state.ambient_enabled,
                                        idle_dim: state.idle_dim,
                                    });
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            OledCareMsg::SetHideUnsupported(hide) => {
                self.hide_unsupported = hide;
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: OledCareCommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            OledCareCommandOutput::PanelSet(active) => {
                let value = if active { "autohide" } else { "none" };
                tracing::info!("{}", t!("oled_care_panel_set", value = value));
            }
            OledCareCommandOutput::TransparencySet(active) => {
                let value = if active { "transparent" } else { "opaque" };
                tracing::info!("{}", t!("oled_care_transparency_set", value = value));
            }
            OledCareCommandOutput::PixelRefreshSet(active) => {
                let value = if active { "300s" } else { "600s" };
                tracing::info!("{}", t!("oled_care_dpms_set", value = value));
            }
            OledCareCommandOutput::GnomeStateLoaded {
                idle_delay,
                ambient_enabled,
                idle_dim,
            } => {
                self.gnome_system_idle_secs = idle_delay;
                self.rebuild_screen_blank_combo();
                if self.gnome_ambient_configured.is_none()
                    && let Some(enabled) = ambient_enabled
                {
                    self.gnome_ambient_active = enabled;
                }
                if self.gnome_idle_dim_configured.is_none()
                    && let Some(enabled) = idle_dim
                {
                    self.gnome_idle_dim_active = enabled;
                }
                self.sync_gnome_switches();
            }
            OledCareCommandOutput::GnomeScreenBlankSet(secs) => {
                self.gnome_system_idle_secs = Some(secs);
                self.rebuild_screen_blank_combo();
                tracing::info!(
                    "{}",
                    t!("oled_care_screen_blank_set", secs = secs.to_string())
                );
            }
            OledCareCommandOutput::GnomeScreenBlankFailed {
                error,
                rollback_secs,
            } => {
                self.gnome_screen_blank_secs = rollback_secs;
                AppConfig::update(|c| {
                    c.active_profile_mut().gnome_screen_blank_secs = rollback_secs
                });
                self.rebuild_screen_blank_combo();
                let _ = sender.output(error);
            }
            OledCareCommandOutput::GnomeAmbientSet(enabled) => {
                tracing::info!(
                    "{}",
                    t!(
                        "oled_care_ambient_brightness_set",
                        value = enabled.to_string()
                    )
                );
            }
            OledCareCommandOutput::GnomeAmbientFailed {
                error,
                rollback_active,
                rollback_configured,
            } => {
                self.gnome_ambient_active = rollback_active;
                self.gnome_ambient_configured = rollback_configured;
                AppConfig::update(|c| {
                    c.active_profile_mut().gnome_ambient_brightness = rollback_configured
                });
                self.sync_gnome_switches();
                let _ = sender.output(error);
            }
            OledCareCommandOutput::GnomeIdleDimSet(enabled) => {
                tracing::info!(
                    "{}",
                    t!("oled_care_idle_dim_set", value = enabled.to_string())
                );
            }
            OledCareCommandOutput::GnomeIdleDimFailed {
                error,
                rollback_active,
                rollback_configured,
            } => {
                self.gnome_idle_dim_active = rollback_active;
                self.gnome_idle_dim_configured = rollback_configured;
                AppConfig::update(|c| {
                    c.active_profile_mut().gnome_idle_dim = rollback_configured
                });
                self.sync_gnome_switches();
                let _ = sender.output(error);
            }
            OledCareCommandOutput::Error(e) => {
                let _ = sender.output(e);
            }
        }
    }
}

async fn read_gsettings(schema: &str, key: &str) -> Option<String> {
    let out = tokio::process::Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

struct GnomeOledCareState {
    idle_delay: Option<u32>,
    ambient_enabled: Option<bool>,
    idle_dim: Option<bool>,
}

async fn read_gnome_oled_care_state() -> GnomeOledCareState {
    let idle_raw = read_gsettings("org.gnome.desktop.session", "idle-delay").await;
    let ambient_raw =
        read_gsettings("org.gnome.settings-daemon.plugins.power", "ambient-enabled").await;
    let idle_dim_raw =
        read_gsettings("org.gnome.settings-daemon.plugins.power", "idle-dim").await;
    GnomeOledCareState {
        idle_delay: idle_raw.as_deref().and_then(parse_gsettings_uint32),
        ambient_enabled: ambient_raw.as_deref().and_then(parse_gsettings_bool),
        idle_dim: idle_dim_raw.as_deref().and_then(parse_gsettings_bool),
    }
}

async fn set_gnome_idle_delay(secs: u32) -> Result<(), String> {
    let val = secs.to_string();
    run_command_blocking("gsettings", &["set", "org.gnome.desktop.session", "idle-delay", &val])
        .await
}

async fn set_gnome_ambient_enabled(enabled: bool) -> Result<(), String> {
    let val = if enabled { "true" } else { "false" };
    run_command_blocking(
        "gsettings",
        &[
            "set",
            "org.gnome.settings-daemon.plugins.power",
            "ambient-enabled",
            val,
        ],
    )
    .await
}

async fn set_gnome_idle_dim(enabled: bool) -> Result<(), String> {
    let val = if enabled { "true" } else { "false" };
    run_command_blocking(
        "gsettings",
        &[
            "set",
            "org.gnome.settings-daemon.plugins.power",
            "idle-dim",
            val,
        ],
    )
    .await
}

async fn plasmashell_evaluate(
    script: &str,
    out: &relm4::Sender<OledCareCommandOutput>,
    success_output: OledCareCommandOutput,
) {
    let args = vec![
        "org.kde.plasmashell".to_string(),
        "/PlasmaShell".to_string(),
        "org.kde.PlasmaShell.evaluateScript".to_string(),
        script.to_string(),
    ];
    match run_qdbus(args).await {
        Ok(()) => out.emit(success_output),
        Err(e) => out.emit(OledCareCommandOutput::Error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gsettings_uint32() {
        assert_eq!(parse_gsettings_uint32("uint32 180\n"), Some(180));
        assert_eq!(parse_gsettings_uint32("uint32 0"), Some(0));
        assert_eq!(parse_gsettings_uint32("300"), Some(300));
        assert_eq!(parse_gsettings_uint32("uint32 -1"), None);
        assert_eq!(parse_gsettings_uint32(""), None);
    }

    #[test]
    fn test_parse_gsettings_bool() {
        assert_eq!(parse_gsettings_bool("true\n"), Some(true));
        assert_eq!(parse_gsettings_bool(" false "), Some(false));
        assert_eq!(parse_gsettings_bool("yes"), None);
        assert_eq!(parse_gsettings_bool(""), None);
    }
}
