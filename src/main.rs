mod appinfo;
mod classify;
mod command;
mod gamedb;
mod hid;
mod i18n;
mod protocol;
mod router;
mod state;
mod tray;

use state::{Action, State};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(60);

fn main() {
    if std::env::args().nth(1).as_deref() == Some("update-db") {
        match gamedb::update_db() {
            Ok(n) => println!("{}", i18n::tr().f("updatedb.done", &[("n", &n.to_string())])),
            Err(e) => {
                eprintln!("{}", i18n::tr().f("updatedb.failed", &[("err", &e)]));
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
    let tray = tray::spawn_tray(state.clone());

    for ev in rx {
        if state.needs_status(&ev) {
            let _ = hid::request_status();
        }
        for action in state.apply(ev) {
            match action {
                Action::Alert(p) => tray::notify_low(p),
            }
        }
        tray.update(state.clone());
    }
}
