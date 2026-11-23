//! Headset state and what to do when it changes.

use crate::protocol::Event;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub connected: bool,
    pub battery: Option<u8>,
    pub charging: bool,
    armed20: bool,
    armed10: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            connected: false,
            battery: None,
            charging: false,
            armed20: true,
            armed10: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Alert(u8),
}

impl State {
    pub fn apply(&mut self, ev: Event) -> Vec<Action> {
        let mut out = Vec::new();
        match ev {
            Event::Status { connected, battery, charging } => {
                self.set_connected(connected);
                if connected {
                    self.set_charging(charging);
                    self.set_battery(battery, &mut out);
                }
            }
            Event::Battery(b) => self.set_battery(b, &mut out),
            Event::Charging(c) => self.set_charging(c),
            Event::Connected(c) => self.set_connected(c),
        }
        out
    }

    fn set_connected(&mut self, c: bool) {
        self.connected = c;
    }

    fn set_charging(&mut self, c: bool) {
        if c {
            self.armed20 = true;
            self.armed10 = true;
        }
        self.charging = c;
    }

    fn set_battery(&mut self, b: u8, out: &mut Vec<Action>) {
        self.battery = Some(b);
        if self.charging {
            return;
        }
        if b <= 10 && self.armed10 {
            self.armed10 = false;
            self.armed20 = false;
            out.push(Action::Alert(10));
        } else if b <= 20 && self.armed20 {
            self.armed20 = false;
            out.push(Action::Alert(20));
        }
    }

    /// Should the daemon ask for a fresh `B0` before applying `ev`? True when the
    /// headset reappears (or talks while we think it is off), so battery
    /// and charging are resynced without waiting for the poll.
    pub fn needs_status(&self, ev: &Event) -> bool {
        match ev {
            Event::Connected(c) => *c,
            Event::Status { .. } => false,
            _ => !self.connected,
        }
    }

    pub fn tooltip(&self) -> String {
        self.tooltip_in(crate::i18n::tr())
    }

    pub fn tooltip_in(&self, tr: &crate::i18n::Tr) -> String {
        match (self.connected, self.battery) {
            (false, _) => tr.t("status.off"),
            (true, None) => tr.t("status.checking"),
            (true, Some(b)) if self.charging => tr.f("status.battery_charging", &[("pct", &b.to_string())]),
            (true, Some(b)) => tr.f("status.battery", &[("pct", &b.to_string())]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Tr;
    use crate::protocol::Event;

    fn online(battery: u8) -> State {
        let mut s = State::default();
        s.apply(Event::Status { connected: true, battery, charging: false });
        s
    }

    #[test]
    fn offline_status() {
        let mut s = State::default();
        let a = s.apply(Event::Status { connected: false, battery: 0, charging: false });
        assert!(a.is_empty());
        assert_eq!(s.tooltip_in(&Tr::for_lang("ko")), "헤드셋 꺼짐");
    }

    #[test]
    fn alert_once_per_threshold() {
        let mut s = online(21);
        assert_eq!(s.apply(Event::Battery(20)), vec![Action::Alert(20)]);
        assert_eq!(s.apply(Event::Battery(19)), vec![]);
        assert_eq!(s.apply(Event::Battery(21)), vec![]);
        assert_eq!(s.apply(Event::Battery(20)), vec![]);
        assert_eq!(s.apply(Event::Battery(10)), vec![Action::Alert(10)]);
    }

    #[test]
    fn alert_rearms_on_charge() {
        let mut s = online(19);
        s.apply(Event::Charging(true));
        s.apply(Event::Charging(false));
        assert_eq!(s.apply(Event::Battery(20)), vec![Action::Alert(20)]);
    }

    #[test]
    fn no_alert_while_charging() {
        let mut s = online(30);
        s.apply(Event::Charging(true));
        assert_eq!(s.apply(Event::Battery(15)), vec![]);
    }

    #[test]
    fn skip_to_ten() {
        let mut s = online(25);
        assert_eq!(s.apply(Event::Battery(9)), vec![Action::Alert(10)]);
    }

    #[test]
    fn tooltip_formats() {
        let (ko, en) = (Tr::for_lang("ko"), Tr::for_lang("en"));
        let mut s = online(85);
        assert_eq!(s.tooltip_in(&ko), "배터리 85%");
        assert_eq!(s.tooltip_in(&en), "Battery 85%");
        s.apply(Event::Charging(true));
        assert_eq!(s.tooltip_in(&ko), "배터리 85% (충전 중)");
        assert_eq!(s.tooltip_in(&en), "Battery 85% (charging)");
        s.apply(Event::Connected(false));
        assert_eq!(s.tooltip_in(&ko), "헤드셋 꺼짐");
        assert_eq!(s.tooltip_in(&en), "Headset off");
        let c = State { connected: true, ..State::default() };
        assert_eq!(c.tooltip_in(&ko), "확인 중…");
        assert_eq!(c.tooltip_in(&en), "Checking…");
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn needs_status_when_headset_comes_back() {
        let s = State::default(); // not connected
        assert!(s.needs_status(&Event::Connected(true)));
        assert!(s.needs_status(&Event::Battery(50)));
        assert!(!s.needs_status(&Event::Connected(false)));
        let st = Event::Status { connected: false, battery: 0, charging: false };
        assert!(!s.needs_status(&st));
    }

    #[test]
    fn no_extra_status_while_connected() {
        let mut s = State::default();
        s.apply(Event::Status { connected: true, battery: 50, charging: false });
        assert!(!s.needs_status(&Event::Battery(49)));
        assert!(s.needs_status(&Event::Connected(true)));
    }
}
