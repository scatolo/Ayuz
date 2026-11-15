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

use crate::services::commands::{is_gnome_desktop, is_kde_desktop};
use crate::services::config::AppConfig;
use crate::services::gnome_display::{self, InternalDisplayInfo};

/// State for the built-in display refresh rate component (60 Hz / 120 Hz).
pub struct RefreshRateModel {
    desktop_supported: bool,
    supported: bool,
    available_hz: Vec<u32>,
    active_hz: Option<u32>,
    selected_hz: Option<u32>,
    resolution_label: String,
    combo_row: adw::ComboRow,
    suppress_signals: Rc<Cell<bool>>,
}

#[derive(Debug)]
pub enum RefreshRateMsg {
    SelectIndex(u32),
    LoadProfile(Option<u32>),
    Refresh,
}

#[derive(Debug)]
pub enum RefreshRateCommandOutput {
    InfoLoaded(Result<InternalDisplayInfo, String>),
    RateApplied {
        hz: u32,
        is_profile_load: bool,
    },
    ApplyFailed {
        error: String,
        rollback_hz: Option<Option<u32>>,
    },
}

impl RefreshRateModel {
    fn rebuild_combo(&self) {
        let mut labels: Vec<String> = Vec::with_capacity(1 + self.available_hz.len());
        let keep_label = match self.active_hz {
            Some(hz) => t!("refresh_rate_keep_current_active", hz = hz.to_string()).to_string(),
            None => t!("refresh_rate_keep_current").to_string(),
        };
        labels.push(keep_label);
        for &hz in &self.available_hz {
            labels.push(format!("{hz} Hz"));
        }

        let str_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected_idx = match self.selected_hz {
            None => 0,
            Some(hz) => self
                .available_hz
                .iter()
                .position(|&x| x == hz)
                .map(|i| (i + 1) as u32)
                .unwrap_or(0),
        };

        self.suppress_signals.set(true);
        self.combo_row.set_model(Some(&gtk::StringList::new(&str_refs)));
        self.combo_row.set_selected(selected_idx);
        self.suppress_signals.set(false);
    }

    fn subtitle_text(&self) -> String {
        if let Some(hz) = self.active_hz {
            if self.resolution_label.is_empty() {
                t!("refresh_rate_active_only", hz = hz.to_string()).to_string()
            } else {
                t!(
                    "refresh_rate_active_res",
                    hz = hz.to_string(),
                    res = self.resolution_label
                )
                .to_string()
            }
        } else {
            t!("refresh_rate_subtitle").to_string()
        }
    }
}

#[relm4::component(pub)]
impl Component for RefreshRateModel {
    type Init = ();
    type Input = RefreshRateMsg;
    type Output = String;
    type CommandOutput = RefreshRateCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &t!("refresh_rate_group_title"),
            set_description: Some(&t!("refresh_rate_group_desc")),

            #[template]
            add = &crate::components::widgets::DaemonWarningLabel {
                #[watch]
                set_visible: !model.desktop_supported,
                set_label: &t!("refresh_rate_desktop_required"),
            },

            add = &model.combo_row.clone() -> adw::ComboRow {
                set_title: &t!("refresh_rate_title"),
                #[watch]
                set_subtitle: &model.subtitle_text(),
                #[watch]
                set_sensitive: model.supported && model.available_hz.len() > 1,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let desktop_supported = is_gnome_desktop() || is_kde_desktop();
        let saved_hz = AppConfig::load().active_profile().display_refresh_hz;

        let combo_row = adw::ComboRow::new();
        let suppress_signals = Rc::new(Cell::new(false));
        {
            let sender = sender.clone();
            let suppress = suppress_signals.clone();
            combo_row.connect_selected_notify(move |row| {
                if suppress.get() {
                    return;
                }
                sender.input(RefreshRateMsg::SelectIndex(row.selected()));
            });
        }

        let model = RefreshRateModel {
            desktop_supported,
            supported: false,
            available_hz: Vec::new(),
            active_hz: None,
            selected_hz: saved_hz,
            resolution_label: String::new(),
            combo_row,
            suppress_signals,
        };
        model.rebuild_combo();

        let widgets = view_output!();

        if desktop_supported {
            sender.command(|out, shutdown| {
                shutdown
                    .register(async move {
                        let res = gnome_display::get_internal_display_info().await;
                        out.emit(RefreshRateCommandOutput::InfoLoaded(res));
                    })
                    .drop_on_shutdown()
            });
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: RefreshRateMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            RefreshRateMsg::Refresh => {
                if !self.desktop_supported {
                    return;
                }
                sender.command(|out, shutdown| {
                    shutdown
                        .register(async move {
                            let res = gnome_display::get_internal_display_info().await;
                            out.emit(RefreshRateCommandOutput::InfoLoaded(res));
                        })
                        .drop_on_shutdown()
                });
            }
            RefreshRateMsg::SelectIndex(idx) => {
                let new_hz = if idx == 0 {
                    None
                } else {
                    self.available_hz.get((idx - 1) as usize).copied()
                };

                if new_hz == self.selected_hz {
                    return;
                }

                let prev_hz = self.selected_hz;
                self.selected_hz = new_hz;
                AppConfig::update(|c| c.active_profile_mut().display_refresh_hz = new_hz);

                if let Some(target_hz) = new_hz {
                    if !self.supported {
                        return;
                    }
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match gnome_display::set_internal_refresh_rate(target_hz).await {
                                    Ok(applied) => out.emit(RefreshRateCommandOutput::RateApplied {
                                        hz: applied,
                                        is_profile_load: false,
                                    }),
                                    Err(error) => out.emit(RefreshRateCommandOutput::ApplyFailed {
                                        error,
                                        rollback_hz: Some(prev_hz),
                                    }),
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            RefreshRateMsg::LoadProfile(target_hz) => {
                self.selected_hz = target_hz;
                self.rebuild_combo();

                if let Some(hz) = target_hz
                    && self.supported
                    && self.available_hz.contains(&hz)
                {
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match gnome_display::set_internal_refresh_rate(hz).await {
                                    Ok(applied) => out.emit(RefreshRateCommandOutput::RateApplied {
                                        hz: applied,
                                        is_profile_load: true,
                                    }),
                                    Err(error) => out.emit(RefreshRateCommandOutput::ApplyFailed {
                                        error,
                                        rollback_hz: None,
                                    }),
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: RefreshRateCommandOutput,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match msg {
            RefreshRateCommandOutput::InfoLoaded(Ok(info)) => {
                self.supported = !info.available_hz.is_empty();
                self.available_hz = info.available_hz.clone();
                self.active_hz = Some(info.current_hz);
                self.resolution_label = format!("{}×{}", info.current_width, info.current_height);
                self.rebuild_combo();

                if let Some(target_hz) = self.selected_hz
                    && info.available_hz.contains(&target_hz)
                    && info.current_hz != target_hz
                {
                    sender.command(move |out, shutdown| {
                        shutdown
                            .register(async move {
                                match gnome_display::set_internal_refresh_rate(target_hz).await {
                                    Ok(applied) => out.emit(RefreshRateCommandOutput::RateApplied {
                                        hz: applied,
                                        is_profile_load: true,
                                    }),
                                    Err(error) => out.emit(RefreshRateCommandOutput::ApplyFailed {
                                        error,
                                        rollback_hz: None,
                                    }),
                                }
                            })
                            .drop_on_shutdown()
                    });
                }
            }
            RefreshRateCommandOutput::InfoLoaded(Err(e)) => {
                tracing::debug!("Display refresh rate discovery unavailable: {e}");
                self.supported = false;
                self.available_hz.clear();
                self.active_hz = None;
                self.rebuild_combo();
            }
            RefreshRateCommandOutput::RateApplied { hz, is_profile_load } => {
                self.active_hz = Some(hz);
                self.rebuild_combo();
                if !is_profile_load {
                    tracing::info!("{}", t!("refresh_rate_set", hz = hz.to_string()));
                }
            }
            RefreshRateCommandOutput::ApplyFailed { error, rollback_hz } => {
                if let Some(prev) = rollback_hz {
                    self.selected_hz = prev;
                    AppConfig::update(|c| c.active_profile_mut().display_refresh_hz = prev);
                    self.rebuild_combo();
                }
                let _ = sender.output(error);
            }
        }
    }
}
