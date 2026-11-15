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
use std::time::Duration;

use gtk4 as gtk;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::prelude::*;
use rust_i18n::t;

use crate::services::{commands, dbus};
use crate::sys_paths;

const UINPUT_UDEV_FIX_CMD: &str = "echo 'KERNEL==\"uinput\", MODE=\"0660\", GROUP=\"input\", OPTIONS+=\"static_node=uinput\"' | sudo tee /etc/udev/rules.d/99-ayuz-uinput.rules && sudo udevadm control --reload-rules && sudo udevadm trigger /dev/uinput";

#[zbus::proxy(
    interface = "net.hadess.SensorProxy",
    default_service = "net.hadess.SensorProxy",
    default_path = "/net/hadess/SensorProxy"
)]
trait SensorProxyCheck {
    #[zbus(property)]
    fn has_ambient_light(&self) -> zbus::Result<bool>;
}

async fn check_sensor_proxy_available() -> bool {
    let conn = match zbus::Connection::system().await {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Ok(proxy) = SensorProxyCheckProxy::new(&conn).await {
        proxy.has_ambient_light().await.is_ok()
    } else {
        false
    }
}

/// Raw probe results gathered off the GTK main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticProbe {
    pub asusd_available: bool,
    pub has_input_group: bool,
    pub can_read_input_events: bool,
    pub uinput_writable: bool,
    pub playerctl_installed: bool,
    pub easyeffects_installed: bool,
    pub sensor_proxy_dbus_available: bool,
    pub monitor_sensor_installed: bool,
    pub playerctl_install_cmd: String,
    pub easyeffects_install_cmd: String,
    pub sensor_proxy_install_cmd: String,
}

/// UI-ready row data for a single diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticItem {
    pub id: &'static str,
    pub title: String,
    pub status_text: String,
    pub ok: bool,
    pub fix_command: Option<String>,
}

/// Converts raw system probe flags into localized diagnostic rows and exact fix commands.
pub fn build_diagnostic_items(probe: &DiagnosticProbe) -> Vec<DiagnosticItem> {
    let mut items = Vec::with_capacity(6);

    items.push(DiagnosticItem {
        id: "asusd",
        title: t!("diagnostics_item_asusd").to_string(),
        status_text: if probe.asusd_available {
            t!("diagnostics_status_active").to_string()
        } else {
            t!("diagnostics_status_asusd_inactive").to_string()
        },
        ok: probe.asusd_available,
        fix_command: (!probe.asusd_available)
            .then(|| "sudo systemctl enable --now asusd".to_string()),
    });

    let input_ok = probe.has_input_group && probe.can_read_input_events;
    let (input_status, input_fix) = if input_ok {
        (t!("diagnostics_status_ready").to_string(), None)
    } else if probe.has_input_group {
        (
            t!("diagnostics_status_input_relogin").to_string(),
            Some("sudo usermod -aG input $USER".to_string()),
        )
    } else {
        (
            t!("diagnostics_status_input_missing").to_string(),
            Some("sudo usermod -aG input $USER".to_string()),
        )
    };
    items.push(DiagnosticItem {
        id: "input",
        title: t!("diagnostics_item_input").to_string(),
        status_text: input_status,
        ok: input_ok,
        fix_command: input_fix,
    });

    items.push(DiagnosticItem {
        id: "uinput",
        title: t!("diagnostics_item_uinput").to_string(),
        status_text: if probe.uinput_writable {
            t!("diagnostics_status_writable").to_string()
        } else {
            t!("diagnostics_status_uinput_denied").to_string()
        },
        ok: probe.uinput_writable,
        fix_command: (!probe.uinput_writable).then(|| UINPUT_UDEV_FIX_CMD.to_string()),
    });

    items.push(DiagnosticItem {
        id: "playerctl",
        title: t!("diagnostics_item_playerctl").to_string(),
        status_text: if probe.playerctl_installed {
            t!("diagnostics_status_installed").to_string()
        } else {
            t!("diagnostics_status_missing").to_string()
        },
        ok: probe.playerctl_installed,
        fix_command: (!probe.playerctl_installed).then(|| probe.playerctl_install_cmd.clone()),
    });

    items.push(DiagnosticItem {
        id: "easyeffects",
        title: t!("diagnostics_item_easyeffects").to_string(),
        status_text: if probe.easyeffects_installed {
            t!("diagnostics_status_installed").to_string()
        } else {
            t!("diagnostics_status_missing").to_string()
        },
        ok: probe.easyeffects_installed,
        fix_command: (!probe.easyeffects_installed)
            .then(|| probe.easyeffects_install_cmd.clone()),
    });

    let (sensor_status, sensor_fix) = if probe.sensor_proxy_dbus_available {
        (t!("diagnostics_status_active").to_string(), None)
    } else if probe.monitor_sensor_installed {
        (
            t!("diagnostics_status_service_inactive").to_string(),
            Some("sudo systemctl enable --now iio-sensor-proxy".to_string()),
        )
    } else {
        (
            t!("diagnostics_status_missing").to_string(),
            Some(probe.sensor_proxy_install_cmd.clone()),
        )
    };
    items.push(DiagnosticItem {
        id: "iio-sensor-proxy",
        title: t!("diagnostics_item_sensor_proxy").to_string(),
        status_text: sensor_status,
        ok: probe.sensor_proxy_dbus_available,
        fix_command: sensor_fix,
    });

    items
}

async fn run_probe() -> DiagnosticProbe {
    let (
        asusd_available,
        sensor_proxy_dbus_available,
        playerctl_installed,
        easyeffects_installed,
        monitor_sensor_installed,
    ) = tokio::join!(
        async {
            tokio::time::timeout(Duration::from_secs(3), dbus::check_asusd_available())
                .await
                .unwrap_or(false)
        },
        async {
            tokio::time::timeout(Duration::from_secs(3), check_sensor_proxy_available())
                .await
                .unwrap_or(false)
        },
        commands::which_exists("playerctl"),
        commands::which_exists("easyeffects"),
        commands::which_exists("monitor-sensor"),
    );

    tokio::task::spawn_blocking(move || DiagnosticProbe {
        asusd_available,
        has_input_group: sys_paths::has_input_group(),
        can_read_input_events: sys_paths::can_read_any_input_event(),
        uinput_writable: sys_paths::is_path_writable(Path::new(sys_paths::DEV_UINPUT)),
        playerctl_installed,
        easyeffects_installed,
        sensor_proxy_dbus_available,
        monitor_sensor_installed,
        playerctl_install_cmd: sys_paths::package_install_command(
            "playerctl",
            "playerctl",
            "playerctl",
        ),
        easyeffects_install_cmd: sys_paths::package_install_command(
            "easyeffects",
            "easyeffects",
            "easyeffects",
        ),
        sensor_proxy_install_cmd: sys_paths::package_install_command(
            "iio-sensor-proxy",
            "iio-sensor-proxy",
            "iio-sensor-proxy",
        ),
    })
    .await
    .unwrap_or(DiagnosticProbe {
        asusd_available,
        has_input_group: false,
        can_read_input_events: false,
        uinput_writable: false,
        playerctl_installed,
        easyeffects_installed,
        sensor_proxy_dbus_available,
        monitor_sensor_installed,
        playerctl_install_cmd: "sudo dnf install playerctl".to_string(),
        easyeffects_install_cmd: "sudo dnf install easyeffects".to_string(),
        sensor_proxy_install_cmd: "sudo dnf install iio-sensor-proxy".to_string(),
    })
}

pub struct DiagnosticsModel {
    items: Vec<DiagnosticItem>,
    rows: Vec<adw::ActionRow>,
    refreshing: bool,
}

#[derive(Debug)]
pub enum DiagnosticsMsg {
    Refresh,
}

#[derive(Debug)]
pub enum DiagnosticsOutput {
    Toast(String),
}

#[derive(Debug)]
pub enum DiagnosticsCommandOutput {
    Probed(Vec<DiagnosticItem>),
}

impl DiagnosticsModel {
    fn rebuild_rows(&mut self, root: &adw::PreferencesGroup, sender: &ComponentSender<Self>) {
        for row in self.rows.drain(..) {
            root.remove(&row);
        }

        for item in &self.items {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&item.title));

            let subtitle = match &item.fix_command {
                Some(cmd) => format!("{} — {}", item.status_text, cmd),
                None => item.status_text.clone(),
            };
            row.set_subtitle(&gtk::glib::markup_escape_text(&subtitle));

            let status_icon = gtk::Image::from_icon_name(if item.ok {
                "emblem-ok-symbolic"
            } else {
                "dialog-warning-symbolic"
            });
            status_icon.set_valign(gtk::Align::Center);
            if item.ok {
                status_icon.add_css_class("success");
            } else {
                status_icon.add_css_class("warning");
            }
            row.add_prefix(&status_icon);

            if let Some(cmd) = item.fix_command.clone() {
                let copy_btn = gtk::Button::from_icon_name("edit-copy-symbolic");
                copy_btn.add_css_class("flat");
                copy_btn.set_valign(gtk::Align::Center);
                copy_btn.set_tooltip_text(Some(&t!("diagnostics_copy_tooltip")));
                let sender_c = sender.clone();
                copy_btn.connect_clicked(move |btn| {
                    btn.clipboard().set_text(&cmd);
                    let _ = sender_c.output(DiagnosticsOutput::Toast(
                        t!("diagnostics_copied_toast").to_string(),
                    ));
                });
                row.add_suffix(&copy_btn);
            }

            root.add(&row);
            self.rows.push(row);
        }
    }

    fn spawn_probe(sender: &ComponentSender<Self>) {
        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    let probe = run_probe().await;
                    let items = build_diagnostic_items(&probe);
                    out.emit(DiagnosticsCommandOutput::Probed(items));
                })
                .drop_on_shutdown()
        });
    }
}

#[relm4::component(pub)]
impl Component for DiagnosticsModel {
    type Init = ();
    type Input = DiagnosticsMsg;
    type Output = DiagnosticsOutput;
    type CommandOutput = DiagnosticsCommandOutput;

    view! {
        #[root]
        adw::PreferencesGroup {
            set_title: &gtk::glib::markup_escape_text(&t!("diagnostics_group_title")),
            set_description: Some(&t!("diagnostics_group_desc")),

            #[wrap(Some)]
            set_header_suffix = &gtk::Button {
                set_icon_name: "view-refresh-symbolic",
                add_css_class: "flat",
                set_valign: gtk::Align::Center,
                set_tooltip_text: Some(&t!("diagnostics_refresh_tooltip")),
                #[watch]
                set_sensitive: !model.refreshing,
                connect_clicked => DiagnosticsMsg::Refresh,
            },
        }
    }

    fn init(
        _init: Self::Init,
        _root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = DiagnosticsModel {
            items: Vec::new(),
            rows: Vec::new(),
            refreshing: true,
        };
        let widgets = view_output!();
        Self::spawn_probe(&sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: DiagnosticsMsg, sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            DiagnosticsMsg::Refresh => {
                if self.refreshing {
                    return;
                }
                self.refreshing = true;
                Self::spawn_probe(&sender);
            }
        }
    }

    fn update_cmd(
        &mut self,
        msg: DiagnosticsCommandOutput,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match msg {
            DiagnosticsCommandOutput::Probed(items) => {
                self.refreshing = false;
                self.items = items;
                self.rebuild_rows(root, &sender);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_diagnostic_items_all_ok_has_no_fix_commands() {
        let probe = DiagnosticProbe {
            asusd_available: true,
            has_input_group: true,
            can_read_input_events: true,
            uinput_writable: true,
            playerctl_installed: true,
            easyeffects_installed: true,
            sensor_proxy_dbus_available: true,
            monitor_sensor_installed: true,
            playerctl_install_cmd: "sudo dnf install playerctl".to_string(),
            easyeffects_install_cmd: "sudo dnf install easyeffects".to_string(),
            sensor_proxy_install_cmd: "sudo dnf install iio-sensor-proxy".to_string(),
        };
        let items = build_diagnostic_items(&probe);
        assert_eq!(items.len(), 6);
        assert!(items.iter().all(|item| item.ok && item.fix_command.is_none()));
    }

    #[test]
    fn test_build_diagnostic_items_reports_exact_fix_commands() {
        let probe = DiagnosticProbe {
            asusd_available: false,
            has_input_group: true,
            can_read_input_events: false,
            uinput_writable: false,
            playerctl_installed: false,
            easyeffects_installed: false,
            sensor_proxy_dbus_available: false,
            monitor_sensor_installed: true,
            playerctl_install_cmd: "sudo dnf install playerctl".to_string(),
            easyeffects_install_cmd: "sudo dnf install easyeffects".to_string(),
            sensor_proxy_install_cmd: "sudo dnf install iio-sensor-proxy".to_string(),
        };
        let items = build_diagnostic_items(&probe);
        assert_eq!(items.len(), 6);
        assert!(items.iter().all(|item| !item.ok));

        assert_eq!(
            items[0].fix_command.as_deref(),
            Some("sudo systemctl enable --now asusd")
        );
        assert_eq!(
            items[1].fix_command.as_deref(),
            Some("sudo usermod -aG input $USER")
        );
        assert_eq!(
            items[2].fix_command.as_deref(),
            Some(UINPUT_UDEV_FIX_CMD)
        );
        assert_eq!(
            items[3].fix_command.as_deref(),
            Some("sudo dnf install playerctl")
        );
        assert_eq!(
            items[4].fix_command.as_deref(),
            Some("sudo dnf install easyeffects")
        );
        assert_eq!(
            items[5].fix_command.as_deref(),
            Some("sudo systemctl enable --now iio-sensor-proxy")
        );
    }

    #[test]
    fn test_build_diagnostic_items_requires_input_group_even_if_some_event_readable() {
        let probe = DiagnosticProbe {
            asusd_available: true,
            has_input_group: false,
            can_read_input_events: true,
            uinput_writable: true,
            playerctl_installed: true,
            easyeffects_installed: true,
            sensor_proxy_dbus_available: true,
            monitor_sensor_installed: true,
            playerctl_install_cmd: "sudo dnf install playerctl".to_string(),
            easyeffects_install_cmd: "sudo dnf install easyeffects".to_string(),
            sensor_proxy_install_cmd: "sudo dnf install iio-sensor-proxy".to_string(),
        };
        let items = build_diagnostic_items(&probe);
        let input_item = items.iter().find(|i| i.id == "input").unwrap();
        assert!(!input_item.ok);
        assert_eq!(
            input_item.fix_command.as_deref(),
            Some("sudo usermod -aG input $USER")
        );
    }
}
