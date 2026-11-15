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

use crate::services::dbus::{self, ArmouryAttributeInfo};
use crate::sys_paths::is_armoury_attr_writable;

/// Control representation chosen for a discovered `AsusArmoury` attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmouryControlKind {
    /// Writable binary `[0, 1]` switch (`boot_sound`, `panel_od`, etc.).
    Toggle,
    /// Writable discrete enumeration (`possible_values`).
    Enum(Vec<i32>),
    /// Read-only status row (`0444` sysfs permissions such as `charge_mode`, or deferred `ppt_*` ranges).
    ReadOnly,
}

/// Discovered `AsusArmoury` attribute combined with its sysfs writability check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmouryEntry {
    pub info: ArmouryAttributeInfo,
    pub writable: bool,
    pub kind: ArmouryControlKind,
}

/// Classifies how an `AsusArmoury` attribute should be presented in the UI.
///
/// Attributes that are read-only in sysfs (`0444`, like `charge_mode` on UX3405CA) or belong to
/// active CPU power/PPT tuning (`ppt_*`) are intentionally kept read-only.
pub fn classify_armoury_attribute(info: &ArmouryAttributeInfo, writable: bool) -> ArmouryControlKind {
    if !writable || info.name.starts_with("ppt_") {
        return ArmouryControlKind::ReadOnly;
    }

    let mut sorted = info.possible_values.clone();
    sorted.sort_unstable();
    sorted.dedup();

    if sorted == [0, 1] || (sorted.is_empty() && info.min_value == Some(0) && info.max_value == Some(1))
    {
        ArmouryControlKind::Toggle
    } else if sorted.len() >= 2 {
        ArmouryControlKind::Enum(sorted)
    } else {
        ArmouryControlKind::ReadOnly
    }
}

/// Formats a human-readable title for a firmware attribute name.
pub fn format_armoury_title(name: &str) -> String {
    match name {
        "boot_sound" => t!("armoury_attr_boot_sound").to_string(),
        "charge_mode" => t!("armoury_attr_charge_mode").to_string(),
        "panel_od" => t!("armoury_attr_panel_od").to_string(),
        "dgpu_disable" => t!("armoury_attr_dgpu_disable").to_string(),
        "gpu_mux_mode" => t!("armoury_attr_gpu_mux_mode").to_string(),
        "mini_led_mode" => t!("armoury_attr_mini_led_mode").to_string(),
        "nv_dynamic_boost" => t!("armoury_attr_nv_dynamic_boost").to_string(),
        "nv_temp_target" => t!("armoury_attr_nv_temp_target").to_string(),
        "ppt_pl1_spl" => t!("armoury_attr_ppt_pl1_spl").to_string(),
        "ppt_pl2_sppt" => t!("armoury_attr_ppt_pl2_sppt").to_string(),
        "ppt_fppt" => t!("armoury_attr_ppt_fppt").to_string(),
        other => other
            .split('_')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => {
                        let up: String = first.to_uppercase().collect();
                        format!("{up}{}", chars.as_str())
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Formats a firmware attribute value for display.
pub fn format_armoury_value(name: &str, value: i32) -> String {
    if name == "charge_mode" {
        match value {
            0 => t!("armoury_val_charge_standard").to_string(),
            1 => t!("armoury_val_charge_fast").to_string(),
            2 => t!("armoury_val_charge_trickle").to_string(),
            _ => value.to_string(),
        }
    } else if name.starts_with("ppt_") {
        format!("{value} W")
    } else {
        value.to_string()
    }
}

/// Formats the subtitle shown for read-only firmware attributes.
pub fn format_armoury_readonly_subtitle(entry: &ArmouryEntry) -> String {
    let cur = format_armoury_value(&entry.info.name, entry.info.current_value);
    if !entry.writable {
        t!("armoury_readonly_value", value = cur).to_string()
    } else if let (Some(min), Some(max)) = (entry.info.min_value, entry.info.max_value) {
        t!(
            "armoury_range_readonly",
            value = cur,
            min = min.to_string(),
            max = max.to_string()
        )
        .to_string()
    } else {
        t!("armoury_current_value", value = cur).to_string()
    }
}

/// Filters out `apu_mem` (handled by `ApuMemModel`) and attaches sysfs writability metadata.
pub fn filter_and_classify_attributes(
    attrs: Vec<ArmouryAttributeInfo>,
    writable_check: impl Fn(&str) -> bool,
) -> Vec<ArmouryEntry> {
    attrs
        .into_iter()
        .filter(|attr| attr.name != "apu_mem")
        .map(|info| {
            let writable = writable_check(&info.name);
            let kind = classify_armoury_attribute(&info, writable);
            ArmouryEntry {
                info,
                writable,
                kind,
            }
        })
        .collect()
}

enum RenderedRow {
    Toggle {
        path: String,
        widget: adw::SwitchRow,
    },
    Enum {
        path: String,
        values: Vec<i32>,
        widget: adw::ComboRow,
    },
    Info {
        path: String,
        widget: adw::ActionRow,
    },
}

impl RenderedRow {
    fn widget(&self) -> gtk::Widget {
        match self {
            Self::Toggle { widget, .. } => widget.clone().upcast(),
            Self::Enum { widget, .. } => widget.clone().upcast(),
            Self::Info { widget, .. } => widget.clone().upcast(),
        }
    }

    fn path(&self) -> &str {
        match self {
            Self::Toggle { path, .. }
            | Self::Enum { path, .. }
            | Self::Info { path, .. } => path,
        }
    }

    fn sync_value(&self, entry: &ArmouryEntry, suppress: &Cell<bool>) {
        match self {
            Self::Toggle { widget, .. } => {
                let target = entry.info.current_value != 0;
                if widget.is_active() != target {
                    suppress.set(true);
                    widget.set_active(target);
                    suppress.set(false);
                }
            }
            Self::Enum { widget, values, .. } => {
                let target_idx = values
                    .iter()
                    .position(|&v| v == entry.info.current_value)
                    .unwrap_or(0) as u32;
                if widget.selected() != target_idx {
                    suppress.set(true);
                    widget.set_selected(target_idx);
                    suppress.set(false);
                }
            }
            Self::Info { widget, .. } => {
                widget.set_subtitle(&gtk::glib::markup_escape_text(
                    &format_armoury_readonly_subtitle(entry),
                ));
            }
        }
    }
}

/// State for the dynamic `xyz.ljones.AsusArmoury` firmware attributes component.
pub struct ArmouryModel {
    entries: Vec<ArmouryEntry>,
    rendered_rows: Vec<RenderedRow>,
    suppress_signals: Rc<Cell<bool>>,
}

impl ArmouryModel {
    fn rows_match_entries(&self, new_entries: &[ArmouryEntry]) -> bool {
        if self.rendered_rows.len() != new_entries.len() || self.entries.len() != new_entries.len() {
            return false;
        }
        self.entries
            .iter()
            .zip(new_entries.iter())
            .all(|(a, b)| a.info.path == b.info.path && a.kind == b.kind && a.writable == b.writable)
    }

    fn rebuild_rows(&mut self, root: &adw::PreferencesGroup, sender: &ComponentSender<Self>) {
        for row in self.rendered_rows.drain(..) {
            root.remove(&row.widget());
        }

        for entry in &self.entries {
            let title = gtk::glib::markup_escape_text(&format_armoury_title(&entry.info.name));
            let safe_name = gtk::glib::markup_escape_text(&entry.info.name);
            let path = entry.info.path.clone();

            let rendered = match &entry.kind {
                ArmouryControlKind::Toggle => {
                    let row = adw::SwitchRow::new();
                    row.set_title(&title);
                    row.set_subtitle(&safe_name);
                    row.set_active(entry.info.current_value != 0);
                    let sender_c = sender.clone();
                    let suppress = self.suppress_signals.clone();
                    let path_c = path.clone();
                    row.connect_active_notify(move |sw| {
                        if suppress.get() {
                            return;
                        }
                        let value = if sw.is_active() { 1 } else { 0 };
                        sender_c.input(ArmouryMsg::SetValue {
                            path: path_c.clone(),
                            value,
                        });
                    });
                    root.add(&row);
                    RenderedRow::Toggle { path, widget: row }
                }
                ArmouryControlKind::Enum(values) => {
                    let labels: Vec<String> = values
                        .iter()
                        .map(|&v| format_armoury_value(&entry.info.name, v))
                        .collect();
                    let str_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                    let selected_idx = values
                        .iter()
                        .position(|&v| v == entry.info.current_value)
                        .unwrap_or(0) as u32;

                    let row = adw::ComboRow::new();
                    row.set_title(&title);
                    row.set_subtitle(&safe_name);
                    row.set_model(Some(&gtk::StringList::new(&str_refs)));
                    row.set_selected(selected_idx);

                    let sender_c = sender.clone();
                    let suppress = self.suppress_signals.clone();
                    let path_c = path.clone();
                    let values_c = values.clone();
                    row.connect_selected_notify(move |combo| {
                        if suppress.get() {
                            return;
                        }
                        if let Some(&value) = values_c.get(combo.selected() as usize) {
                            sender_c.input(ArmouryMsg::SetValue {
                                path: path_c.clone(),
                                value,
                            });
                        }
                    });
                    root.add(&row);
                    RenderedRow::Enum {
                        path,
                        values: values.clone(),
                        widget: row,
                    }
                }
                ArmouryControlKind::ReadOnly => {
                    let row = adw::ActionRow::new();
                    row.set_title(&title);
                    row.set_subtitle(&gtk::glib::markup_escape_text(
                        &format_armoury_readonly_subtitle(entry),
                    ));
                    root.add(&row);
                    RenderedRow::Info { path, widget: row }
                }
            };

            self.rendered_rows.push(rendered);
        }
    }
}

/// Input messages for the dynamic firmware attributes component.
#[derive(Debug)]
pub enum ArmouryMsg {
    /// Re-discover `AsusArmoury` attributes from `asusd` and refresh values.
    Refresh,
    /// Write a new value to a writable `AsusArmoury` attribute.
    SetValue { path: String, value: i32 },
}

/// Async command outputs for `ArmouryModel`.
#[derive(Debug)]
pub enum ArmouryCommandOutput {
    /// Discovered attribute list (excluding `apu_mem`).
    Discovered(Vec<ArmouryEntry>),
    /// Write confirmed by `asusd`.
    ValueSet { path: String, value: i32 },
    /// Write rejected; revert UI control and toast error.
    SetFailed {
        path: String,
        error: String,
        revert_to: i32,
    },
}

fn spawn_discovery(sender: &ComponentSender<ArmouryModel>) {
    sender.command(|out, shutdown| {
        shutdown
            .register(async move {
                let Ok(attrs) = dbus::discover_armoury_attributes().await else {
                    out.emit(ArmouryCommandOutput::Discovered(Vec::new()));
                    return;
                };
                let entries = tokio::task::spawn_blocking(move || {
                    filter_and_classify_attributes(attrs, is_armoury_attr_writable)
                })
                .await
                .unwrap_or_default();
                out.emit(ArmouryCommandOutput::Discovered(entries));
            })
            .drop_on_shutdown()
    });
}

#[relm4::component(pub)]
impl Component for ArmouryModel {
    type Init = ();
    type Input = ArmouryMsg;
    type Output = String;
    type CommandOutput = ArmouryCommandOutput;

    view! {
        adw::PreferencesGroup {
            set_title: &t!("armoury_group_title"),
            set_description: Some(&t!("armoury_group_desc")),
            #[watch]
            set_visible: !model.entries.is_empty(),
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = ArmouryModel {
            entries: Vec::new(),
            rendered_rows: Vec::new(),
            suppress_signals: Rc::new(Cell::new(false)),
        };
        let widgets = view_output!();
        spawn_discovery(&sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: ArmouryMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            ArmouryMsg::Refresh => {
                spawn_discovery(&sender);
            }
            ArmouryMsg::SetValue { path, value } => {
                let Some(entry) = self.entries.iter_mut().find(|e| e.info.path == path) else {
                    return;
                };
                if matches!(entry.kind, ArmouryControlKind::ReadOnly)
                    || entry.info.current_value == value
                {
                    return;
                }
                let prev = entry.info.current_value;
                entry.info.current_value = value;

                sender.command(move |out, shutdown| {
                    shutdown
                        .register(async move {
                            match dbus::set_armoury_value(&path, value).await {
                                Ok(applied) => out.emit(ArmouryCommandOutput::ValueSet {
                                    path,
                                    value: applied,
                                }),
                                Err(error) => out.emit(ArmouryCommandOutput::SetFailed {
                                    path,
                                    error,
                                    revert_to: prev,
                                }),
                            }
                        })
                        .drop_on_shutdown()
                });
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: ArmouryCommandOutput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match msg {
            ArmouryCommandOutput::Discovered(entries) => {
                if self.rows_match_entries(&entries) {
                    self.entries = entries;
                    for (row, entry) in self.rendered_rows.iter().zip(self.entries.iter()) {
                        row.sync_value(entry, &self.suppress_signals);
                    }
                } else {
                    self.entries = entries;
                    self.rebuild_rows(root, &sender);
                }
            }
            ArmouryCommandOutput::ValueSet { path, value } => {
                tracing::info!("Set AsusArmoury {} = {}", path, value);
                if let Some(entry) = self.entries.iter_mut().find(|e| e.info.path == path) {
                    entry.info.current_value = value;
                    if let Some(row) = self.rendered_rows.iter().find(|r| r.path() == path) {
                        row.sync_value(entry, &self.suppress_signals);
                    }
                }
            }
            ArmouryCommandOutput::SetFailed {
                path,
                error,
                revert_to,
            } => {
                if let Some(entry) = self.entries.iter_mut().find(|e| e.info.path == path) {
                    entry.info.current_value = revert_to;
                    if let Some(row) = self.rendered_rows.iter().find(|r| r.path() == path) {
                        row.sync_value(entry, &self.suppress_signals);
                    }
                }
                let _ = sender.output(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_attr(name: &str, current: i32, possible: Vec<i32>, min: Option<i32>, max: Option<i32>) -> ArmouryAttributeInfo {
        ArmouryAttributeInfo {
            name: name.to_string(),
            path: format!("/xyz/ljones/asus_armoury/{name}"),
            current_value: current,
            default_value: None,
            min_value: min,
            max_value: max,
            possible_values: possible,
        }
    }

    #[test]
    fn test_classify_armoury_attribute_rules() {
        let boot_sound = mk_attr("boot_sound", 1, vec![0, 1], Some(0), Some(1));
        assert_eq!(
            classify_armoury_attribute(&boot_sound, true),
            ArmouryControlKind::Toggle
        );
        assert_eq!(
            classify_armoury_attribute(&boot_sound, false),
            ArmouryControlKind::ReadOnly
        );

        let charge_mode = mk_attr("charge_mode", 0, vec![0, 1, 2], Some(0), Some(2));
        assert_eq!(
            classify_armoury_attribute(&charge_mode, false),
            ArmouryControlKind::ReadOnly
        );
        assert_eq!(
            classify_armoury_attribute(&charge_mode, true),
            ArmouryControlKind::Enum(vec![0, 1, 2])
        );

        let ppt = mk_attr("ppt_pl1_spl", 45, vec![], Some(15), Some(115));
        assert_eq!(
            classify_armoury_attribute(&ppt, true),
            ArmouryControlKind::ReadOnly
        );
    }

    #[test]
    fn test_filter_and_classify_excludes_apu_mem() {
        let attrs = vec![
            mk_attr("apu_mem", 4, vec![0, 4, 8], None, None),
            mk_attr("boot_sound", 0, vec![0, 1], None, None),
            mk_attr("charge_mode", 0, vec![0, 1, 2], None, None),
        ];
        let entries = filter_and_classify_attributes(attrs, |name| name == "boot_sound");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].info.name, "boot_sound");
        assert_eq!(entries[0].kind, ArmouryControlKind::Toggle);
        assert_eq!(entries[1].info.name, "charge_mode");
        assert_eq!(entries[1].kind, ArmouryControlKind::ReadOnly);
    }

    #[test]
    fn test_format_armoury_title_fallback_and_value() {
        assert_eq!(format_armoury_title("my_custom_flag"), "My Custom Flag");
        assert_eq!(format_armoury_value("ppt_pl1_spl", 65), "65 W");
        assert_eq!(format_armoury_value("other", 3), "3");
    }
}
