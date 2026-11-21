//! Headset state and what to do when it changes.

use crate::protocol::Event;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub connected: bool,
    pub battery: Option<u8>,
    pub charging: bool,
    pub game: u8,
    pub chat: u8,
    mix_synced: bool,
    armed20: bool,
    armed10: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            connected: false,
            battery: None,
            charging: false,
            game: 100,
            chat: 100,
            mix_synced: false,
            armed20: true,
            armed10: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SetMix { game: u8, chat: u8 },
    Alert(u8),
}

impl State {
    pub fn apply(&mut self, ev: Event) -> Vec<Action> {
        let mut out = Vec::new();
        match ev {
            Event::Status { connected, battery, charging, game, chat } => {
                self.set_connected(connected);
                if connected {
                    self.set_charging(charging);
                    self.set_battery(battery, &mut out);
                    self.set_mix(game, chat, &mut out);
                }
            }
            Event::ChatMix { game, chat } => {
                if self.connected {
                    self.set_mix(game, chat, &mut out);
                }
            }
            Event::Battery(b) => self.set_battery(b, &mut out),
            Event::Charging(c) => self.set_charging(c),
            Event::Connected(c) => self.set_connected(c),
        }
        out
    }

    fn set_connected(&mut self, c: bool) {
        if !c {
            // Re-apply the mix on the next connection: sinks may hold stale volume.
            self.mix_synced = false;
        }
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

    fn set_mix(&mut self, game: u8, chat: u8, out: &mut Vec<Action>) {
        if self.mix_synced && (game, chat) == (self.game, self.chat) {
            return;
        }
        let first = !self.mix_synced;
        self.mix_synced = true;
        let changed = (game, chat) != (self.game, self.chat);
        self.game = game;
        self.chat = chat;
        if first || changed {
            out.push(Action::SetMix { game, chat });
        }
    }

    /// Should the daemon ask for a fresh `B0` before applying `ev`? True when the
    /// headset reappears (or talks while we think it is off), so battery,
    /// charging and the wheel mix are resynced without waiting for the poll.
    pub fn needs_status(&self, ev: &Event) -> bool {
        match ev {
            Event::Connected(c) => *c,
            Event::Status { .. } => false,
            _ => !self.connected,
        }
    }

    pub fn tooltip(&self) -> String {
        match (self.connected, self.battery) {
            (false, _) => "헤드셋 꺼짐".into(),
            (true, None) => "확인 중…".into(),
            (true, Some(b)) if self.charging => format!("배터리 {b}% (충전 중)"),
            (true, Some(b)) => format!("배터리 {b}%"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Event;

    fn online(battery: u8) -> State {
        let mut s = State::default();
        s.apply(Event::Status { connected: true, battery, charging: false, game: 100, chat: 100 });
        s
    }

    #[test]
    fn offline_status_no_mix() {
        let mut s = State::default();
        let a = s.apply(Event::Status { connected: false, battery: 0, charging: false, game: 30, chat: 70 });
        assert!(a.is_empty());
        assert_eq!(s.tooltip(), "헤드셋 꺼짐");
    }

    #[test]
    fn chatmix_when_connected() {
        let mut s = online(80);
        assert_eq!(s.apply(Event::ChatMix { game: 60, chat: 40 }), vec![Action::SetMix { game: 60, chat: 40 }]);
        assert_eq!(s.apply(Event::ChatMix { game: 60, chat: 40 }), vec![]);
    }

    #[test]
    fn status_mix_change_when_connected() {
        let mut s = online(80);
        let a = s.apply(Event::Status { connected: true, battery: 80, charging: false, game: 50, chat: 100 });
        assert_eq!(a, vec![Action::SetMix { game: 50, chat: 100 }]);
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
        let mut s = online(85);
        assert_eq!(s.tooltip(), "배터리 85%");
        s.apply(Event::Charging(true));
        assert_eq!(s.tooltip(), "배터리 85% (충전 중)");
        s.apply(Event::Connected(false));
        assert_eq!(s.tooltip(), "헤드셋 꺼짐");
        let c = State { connected: true, ..State::default() };
        assert_eq!(c.tooltip(), "확인 중…");
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    #[test]
    fn first_connect_applies_mix() {
        let mut s = State::default();
        let st = Event::Status { connected: true, battery: 80, charging: false, game: 100, chat: 100 };
        assert_eq!(s.apply(st), vec![Action::SetMix { game: 100, chat: 100 }]);
        s.apply(Event::Connected(false));
        assert_eq!(s.apply(st), vec![Action::SetMix { game: 100, chat: 100 }]);
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    #[test]
    fn needs_status_when_headset_comes_back() {
        let s = State::default(); // not connected
        assert!(s.needs_status(&Event::Connected(true)));
        assert!(s.needs_status(&Event::ChatMix { game: 1, chat: 2 }));
        assert!(s.needs_status(&Event::Battery(50)));
        assert!(!s.needs_status(&Event::Connected(false)));
        let st = Event::Status { connected: false, battery: 0, charging: false, game: 0, chat: 0 };
        assert!(!s.needs_status(&st));
    }

    #[test]
    fn no_extra_status_while_connected() {
        let mut s = State::default();
        s.apply(Event::Status { connected: true, battery: 50, charging: false, game: 100, chat: 100 });
        assert!(!s.needs_status(&Event::ChatMix { game: 1, chat: 2 }));
        assert!(!s.needs_status(&Event::Battery(49)));
        assert!(s.needs_status(&Event::Connected(true)));
    }
}
