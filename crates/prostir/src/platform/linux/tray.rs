//! System tray icon (StatusNotifierItem): connection status, video quality,
//! start with the session, quit.

use crate::i18n::t;
use std::path::PathBuf;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{MenuItem, ToolTip};
use log::warn;

use crate::events::EventSink;
use crate::platform::linux::autostart;
use crate::platform::{TrayCommand as Command, TrayStatus as Status};
use prostir_proto::request::{Asked, Command as HeadsetCommand};

/// Asks the headset to do `command`, from the tray.
fn ask(tray: &Tray, command: HeadsetCommand) {
    tray.commands.tray(Command::Headset(Asked::Known(command)));
}

/// Video quality presets offered in the menu, Mbit/s per monitor.
const QUALITIES: [(u32, &str, &str); 3] = [
    (15, "Економна: 15 Мбіт/с", "Economy: 15 Mbit/s"),
    (30, "Звичайна: 30 Мбіт/с", "Normal: 30 Mbit/s"),
    (50, "Висока: 50 Мбіт/с", "High: 50 Mbit/s"),
];

/// The items of the quality menu, and the one checked: a quality none of them has
/// (chosen in the headset, or with `prostir ctl`) is an item of its own, so the menu
/// never shows another one; none is checked before the quality is known (0).
fn qualities(bitrate: u32) -> (usize, Vec<String>) {
    let mut labels: Vec<String> = QUALITIES
        .iter()
        .map(|(_, uk, en)| t(uk, en).into())
        .collect();
    let selected = match QUALITIES.iter().position(|(m, _, _)| *m == bitrate) {
        Some(i) => i,
        None if bitrate == 0 => labels.len(),
        None => {
            labels.push(crate::i18n::count("pc-quality-custom", bitrate as usize));
            labels.len() - 1
        }
    };
    (selected, labels)
}

pub struct Tray {
    status: Status,
    /// Code to type in the headset, while pairing.
    pairing_code: Option<String>,
    /// The settings shown (the main loop says when they change).
    bitrate: u32,
    sharp: bool,
    counting: bool,
    /// The headset's battery: percent, charging.
    battery: Option<(u8, bool)>,
    commands: EventSink,
}

pub type Handle = ksni::blocking::Handle<Tray>;

/// Shows the tray icon; `None` when the desktop has no tray.
pub fn spawn(commands: EventSink) -> Option<Handle> {
    // Rewritten on each start, so it names this install's program even after an
    // upgrade moved it (a Nix store path, say).
    autostart::refresh();
    let tray = Tray {
        status: Status::default(),
        pairing_code: None,
        bitrate: 0,
        sharp: false,
        counting: false,
        battery: None,
        commands,
    };
    tray.spawn()
        .inspect_err(|e| warn!("no system tray, running without an icon: {e}"))
        .ok()
}

/// Updates what the tray shows.
pub fn set_status(handle: Option<&Handle>, status: Status) {
    if let Some(h) = handle {
        h.update(|t| t.status = status);
    }
}

/// The headset's battery as it last reported it (`None`: no headset).
pub fn set_battery(handle: Option<&Handle>, battery: Option<(u8, bool)>) {
    if let Some(h) = handle {
        h.update(|t| t.battery = battery);
    }
}

/// The settings to show: quality, the text preset, the counter.
pub fn set_settings(handle: Option<&Handle>, bitrate: u32, sharp: bool, counting: bool) {
    if let Some(h) = handle {
        h.update(|t| {
            t.bitrate = bitrate;
            t.sharp = sharp;
            t.counting = counting;
        });
    }
}

/// Shows (or hides) the pairing code in the tray.
pub fn set_pairing(handle: Option<&Handle>, code: Option<String>) {
    if let Some(h) = handle {
        h.update(|t| t.pairing_code = code);
    }
}

/// The label of an on/off item. Plasma draws a checkbox; GTK menus (Waybar's tray)
/// often draw nothing for an unchecked item, so there the state goes into the text.
fn with_state(label: &str, on: bool) -> String {
    let plasma = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.contains("KDE"));
    if plasma {
        label.to_owned()
    } else if on {
        format!("{label}: {}", t("увімкнено", "on"))
    } else {
        format!("{label}: {}", t("вимкнено", "off"))
    }
}

/// The app icon when installed (see `scripts/install.sh`), otherwise a stock one.
fn icon() -> String {
    let installed = std::env::var_os("HOME").is_some_and(|h| {
        PathBuf::from(h)
            .join(".local/share/icons/hicolor/scalable/apps/prostir.svg")
            .exists()
    }) || PathBuf::from("/usr/share/icons/hicolor/scalable/apps/prostir.svg")
        .exists();
    if installed {
        "prostir"
    } else {
        "video-display"
    }
    .into()
}

impl Tray {
    fn status_line(&self) -> String {
        match self.status.connected {
            Some(_) if self.status.paused => t(
                "Шолом знято, трансляцію призупинено",
                "Headset taken off, streaming paused",
            )
            .into(),
            Some(via) => format!("{} ({via})", t("Шолом підключено", "Headset connected")),
            None => t("Чекаю на шолом…", "Waiting for the headset…").into(),
        }
    }

    fn monitors_line(&self) -> String {
        let mirror = if self.status.mirror {
            t(", екран ПК у шоломі", ", a PC display in the headset")
        } else {
            ""
        };
        match self.status.monitors {
            0 => t("Віртуальних моніторів немає", "No virtual monitors").into(),
            n => format!(
                "{}: {n}{mirror}",
                t("Віртуальних моніторів", "Virtual monitors")
            ),
        }
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "prostir".into()
    }

    fn title(&self) -> String {
        "Prostir".into()
    }

    fn icon_name(&self) -> String {
        icon()
    }

    /// A click on the icon (a double click where the panel says so): the settings
    /// window opens or comes forward; the menu keeps its own item too.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.commands.tray(Command::clicked());
    }

    fn status(&self) -> ksni::Status {
        // Always visible (a passive icon hides in Plasma's overflow, and a fresh install
        // would show no sign of running at all); a code to read asks for attention.
        if self.pairing_code.is_some() {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: format!("Prostir {}", env!("CARGO_PKG_VERSION")),
            description: match &self.pairing_code {
                Some(code) => format!("{}: {code}", t("Код сполучення", "Pairing code")),
                None => format!("{}\n{}", self.status_line(), self.monitors_line()),
            },
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let (selected, qualities) = qualities(self.bitrate);
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        if let Some(code) = &self.pairing_code {
            items.push(
                StandardItem {
                    label: format!("{}: {code}", t("Код сполучення", "Pairing code")),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            items.push(MenuItem::Separator);
        }
        items.extend([
            StandardItem {
                label: self.status_line(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.monitors_line(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ]);
        if self.status.others > 0 {
            items.push(
                StandardItem {
                    label: format!(
                        "{}: {}",
                        t("Інших шоломів", "Other headsets"),
                        self.status.others
                    ),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.status.desktop_stuck {
            items.push(
                StandardItem {
                    label: t("Стільниця не відповідає", "The desktop does not respond").into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if let Some(encoding) = &self.status.encoding {
            items.push(
                StandardItem {
                    label: encoding.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.status.mic_denied {
            items.push(
                StandardItem {
                    label: t(
                        "Мікрофон шолома: немає дозволу",
                        "Headset microphone: no permission",
                    )
                    .into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if let Some((percent, charging)) = self.battery {
            items.push(
                StandardItem {
                    label: format!(
                        "{} {percent}%{}",
                        t("Заряд шолома:", "Headset battery:"),
                        if charging {
                            t(", заряджається", ", charging")
                        } else {
                            ""
                        }
                    ),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if self.status.connected.is_some() {
            items.push(
                StandardItem {
                    label: t("Монітори перед собою", "Bring the monitors in front").into(),
                    activate: Box::new(|tray: &mut Self| ask(tray, HeadsetCommand::Recenter)),
                    ..Default::default()
                }
                .into(),
            );
        }
        // The headset owns "No limits": without one connected there is nothing to
        // switch.
        let no_limits = self.status.no_limits;
        items.push(
            CheckmarkItem {
                label: with_state(t("Без обмежень", "No limits"), no_limits == Some(true)),
                checked: no_limits == Some(true),
                enabled: no_limits.is_some(),
                activate: Box::new(|tray: &mut Self| {
                    if let Some(on) = tray.status.no_limits {
                        ask(tray, HeadsetCommand::NoLimits { on: !on });
                    }
                }),
                ..Default::default()
            }
            .into(),
        );
        items.extend([
            MenuItem::Separator,
            SubMenu {
                label: t("Якість відео", "Video quality").into(),
                submenu: vec![
                    RadioGroup {
                        selected,
                        // The item of a quality of its own changes nothing.
                        select: Box::new(|tray: &mut Self, i| {
                            if let Some((mbps, _, _)) = QUALITIES.get(i) {
                                tray.commands.tray(Command::SetBitrate(*mbps));
                            }
                        }),
                        options: qualities
                            .into_iter()
                            .map(|label| RadioItem {
                                label,
                                ..Default::default()
                            })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: with_state(
                    t(
                        "Чіткіший текст (кодування на ~2 мс довше)",
                        "Sharper text (about 2 ms more encoding)",
                    ),
                    self.sharp,
                ),
                checked: self.sharp,
                activate: Box::new(|tray: &mut Self| {
                    tray.commands.tray(Command::SetSharp(!tray.sharp));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: with_state(
                    t(
                        "Рахувати цей ПК серед користувачів",
                        "Count this PC among users",
                    ),
                    self.counting,
                ),
                checked: self.counting,
                visible: crate::usage::possible(),
                activate: Box::new(|tray: &mut Self| {
                    tray.commands.tray(Command::SetCounting(!tray.counting));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: t("Запускати разом із системою", "Start with the system").into(),
                checked: autostart::enabled(),
                // Hyprland (without uwsm) ignores XDG autostart: its own config starts it.
                visible: !crate::platform::linux::hyprland::running(),
                // The control loop sets it and keeps the choice (never turned on again).
                activate: Box::new(|tray: &mut Self| {
                    tray.commands
                        .tray(Command::SetAutostart(!autostart::enabled()));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: t("Налаштування…", "Settings…").into(),
                icon_name: "configure".into(),
                activate: Box::new(|tray: &mut Self| {
                    tray.commands.tray(Command::ShowSettings);
                }),
                ..Default::default()
            }
            .into(),
            if self.status.pairing {
                StandardItem {
                    label: t("Зупинити сполучення", "Stop pairing").into(),
                    activate: Box::new(|tray: &mut Self| {
                        tray.commands.tray(Command::StopPairing);
                    }),
                    ..Default::default()
                }
            } else {
                StandardItem {
                    label: t("Сполучити новий шолом", "Pair a new headset").into(),
                    activate: Box::new(|tray: &mut Self| {
                        tray.commands.tray(Command::PairNewHeadset);
                    }),
                    ..Default::default()
                }
            }
            .into(),
            StandardItem {
                label: t("Забути сполучені шоломи", "Forget paired headsets").into(),
                activate: Box::new(|tray: &mut Self| {
                    tray.commands.tray(Command::ForgetHeadsets);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: format!("Prostir {}", env!("CARGO_PKG_VERSION")),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: t("Вийти", "Quit").into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    tray.commands.tray(Command::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quality none of the items has (set from the local API or the headset's
    /// menu) is checked as an item of its own, never as another quality.
    #[test]
    fn a_quality_of_no_item_has_its_own() {
        let (selected, labels) = qualities(30);
        assert_eq!((selected, labels.len()), (1, QUALITIES.len()));
        let (selected, labels) = qualities(20);
        assert_eq!(labels.len(), QUALITIES.len() + 1, "{labels:?}");
        assert_eq!(selected, QUALITIES.len());
        assert!(labels[selected].contains("20"), "{labels:?}");
    }
}
