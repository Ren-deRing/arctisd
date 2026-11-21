mod appinfo;
mod classify;
mod gamedb;
mod hid;
mod mixer;
mod protocol;
mod router;
mod state;
mod tray;

use mixer::{Mixer, Pactl};
use state::{Action, State};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(50);
const POLL: Duration = Duration::from_secs(60);

fn main() {
    if std::env::args().nth(1).as_deref() == Some("update-db") {
        match gamedb::update_db() {
            Ok(n) => println!("arctisd: game DB updated ({n} executables)"),
            Err(e) => {
                eprintln!("arctisd: update-db: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    router::spawn_router();
    let (tx, rx) = mpsc::channel();
    hid::spawn_reader(5, tx.clone());
    hid::spawn_reader(hid::CMD_IFACE, tx);

    // Periodic status refresh in case an event was missed.
    thread::spawn(|| loop {
        let _ = hid::request_status();
        thread::sleep(POLL);
    });

    let mut state = State::default();
    let mut mixer = Mixer::new(Pactl::default(), DEBOUNCE);
    let tray = tray::spawn_tray(state.clone());

    loop {
        let timeout = mixer
            .next_deadline()
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(POLL);
        match rx.recv_timeout(timeout) {
            Ok(ev) => {
                if state.needs_status(&ev) {
                    let _ = hid::request_status();
                }
                for action in state.apply(ev) {
                    match action {
                        Action::SetMix { game, chat } => mixer.submit(game, chat, Instant::now()),
                        Action::Alert(p) => tray::notify_low(p),
                    }
                }
                tray.update(state.clone());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        mixer.flush(Instant::now());
    }
}
