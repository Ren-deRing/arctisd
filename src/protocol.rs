//! Arctis Nova 5X HID protocol.
//!
//! Writes carry a leading report ID 0 (stripped by the kernel); reads arrive
//! without it, so `buf[0]` is the command byte.

/// `00 B0` status request, padded to 64 bytes.
pub const STATUS_REQUEST: [u8; 64] = {
    let mut b = [0u8; 64];
    b[1] = 0xB0;
    b
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Status { connected: bool, battery: u8, charging: bool },
    Battery(u8),
    Charging(bool),
    Connected(bool),
}

const ONLINE: u8 = 0x03;
const CHARGING: u8 = 0x01;

fn pct(b: u8) -> u8 {
    b.min(100)
}

pub fn parse(buf: &[u8]) -> Option<Event> {
    match *buf {
        [0xB0, conn, _, bat, chg, ..] => Some(Event::Status {
            connected: conn == ONLINE,
            battery: pct(bat),
            charging: chg == CHARGING,
        }),
        [0xB7, bat, ..] => Some(Event::Battery(pct(bat))),
        [0xBB, chg, ..] => Some(Event::Charging(chg == CHARGING)),
        [0xB9, conn, ..] => Some(Event::Connected(conn == ONLINE)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_request_bytes() {
        assert_eq!(STATUS_REQUEST.len(), 64);
        assert_eq!(&STATUS_REQUEST[..2], &[0x00, 0xB0]);
        assert!(STATUS_REQUEST[2..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parse_status_online() {
        assert_eq!(
            parse(&[0xB0, 0x03, 0x00, 0x55, 0x01, 0x46, 0x1E]),
            Some(Event::Status { connected: true, battery: 85, charging: true })
        );
    }

    #[test]
    fn parse_status_offline() {
        assert_eq!(
            parse(&[0xB0, 0x02, 0, 0, 0, 100, 100]),
            Some(Event::Status { connected: false, battery: 0, charging: false })
        );
    }

    #[test]
    fn parse_battery_charging_connected() {
        assert_eq!(parse(&[0xB7, 0x14]), Some(Event::Battery(20)));
        assert_eq!(parse(&[0xBB, 0x01]), Some(Event::Charging(true)));
        assert_eq!(parse(&[0xBB, 0x03]), Some(Event::Charging(false)));
        assert_eq!(parse(&[0xB9, 0x03]), Some(Event::Connected(true)));
        assert_eq!(parse(&[0xB9, 0x02]), Some(Event::Connected(false)));
    }

    #[test]
    fn parse_rejects_short_and_unknown() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0xB0, 0x03]), None);
        assert_eq!(parse(&[0xB7]), None);
        assert_eq!(parse(&[0x45, 100, 0]), None);
        assert_eq!(parse(&[0x99, 1, 2]), None);
    }

    #[test]
    fn parse_clamps() {
        assert_eq!(parse(&[0xB7, 0xFF]), Some(Event::Battery(100)));
    }
}
