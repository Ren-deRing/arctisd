//! KDE tray icon and low-battery notification.

use crate::i18n::tr;
use crate::state::State;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;

pub fn icon_name(s: &State) -> &'static str {
    match s.battery {
        _ if !s.connected => "battery-missing",
        _ if s.charging => "battery-good-charging",
        Some(b) if b <= 10 => "battery-caution",
        Some(b) if b <= 20 => "battery-low",
        _ => "battery-good",
    }
}

pub struct Tray {
    state: State,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "arctisd".into()
    }
    fn title(&self) -> String {
        "Arctis".into()
    }
    fn icon_name(&self) -> String {
        icon_name(&self.state).into()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Arctis Nova 5X".into(),
            description: self.state.tooltip(),
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: tr().t("menu.quit"),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub struct TrayHandle(Option<Handle<Tray>>);

impl TrayHandle {
    pub fn update(&self, s: State) {
        if let Some(h) = &self.0 {
            h.update(|t: &mut Tray| t.state = s);
        }
    }
}

/// Without a tray host the daemon still runs (volume control keeps working).
pub fn spawn_tray(initial: State) -> TrayHandle {
    // Without assume_sni_available, spawn() fails for good if the daemon starts
    // before plasmashell's StatusNotifierWatcher.
    match (Tray { state: initial }).assume_sni_available(true).spawn() {
        Ok(h) => TrayHandle(Some(h)),
        Err(e) => {
            eprintln!("arctisd: tray: {e}");
            TrayHandle(None)
        }
    }
}

pub fn notify_low(percent: u8) {
    if let Err(e) = notify_rust::Notification::new()
        .summary(&tr().t("notify.low.summary"))
        .body(&tr().f("notify.low.body", &[("pct", &percent.to_string())]))
        .icon("battery-low")
        .show()
    {
        eprintln!("arctisd: notify: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Event;

    fn st(connected: bool, battery: u8, charging: bool) -> State {
        let mut s = State::default();
        s.apply(Event::Status { connected, battery, charging });
        s
    }

    #[test]
    fn icon_name_cases() {
        assert_eq!(icon_name(&st(false, 0, false)), "battery-missing");
        assert_eq!(icon_name(&st(true, 50, true)), "battery-good-charging");
        assert_eq!(icon_name(&st(true, 10, false)), "battery-caution");
        assert_eq!(icon_name(&st(true, 20, false)), "battery-low");
        assert_eq!(icon_name(&st(true, 21, false)), "battery-good");
    }
}
