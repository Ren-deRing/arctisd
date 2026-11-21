//! Debounced Game/Chat volume control.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Wheel balance is applied to the loopback *streams* (sink-inputs) feeding the
/// headset, not to the virtual sinks: changing a sink's volume makes KDE show its
/// volume OSD, and the sink volume stays the user's own knob.
pub const GAME_TARGET: &str = "arctis_game.out";
pub const CHAT_TARGET: &str = "arctis_chat.out";

pub trait VolumeSink {
    fn set(&mut self, target: &str, percent: u8) -> Result<(), String>;
}

/// Index of the `Sink Input #N` block whose `node.name` is exactly `name`
/// (input: `LC_ALL=C pactl list sink-inputs`).
pub fn find_sink_input(list: &str, name: &str) -> Option<u32> {
    let want = format!("node.name = \"{name}\"");
    let mut current = None;
    for line in list.lines() {
        if let Some(n) = line.strip_prefix("Sink Input #") {
            current = n.trim().parse().ok();
        } else if line.trim() == want {
            return current;
        }
    }
    None
}

const PACTL_TIMEOUT: Duration = Duration::from_secs(1);

/// Run `cmd`, killing it after `timeout` so a wedged pipewire-pulse cannot
/// stall the daemon. Returns stdout on success.
pub fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().expect("piped");
    let reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(st) => break st,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
            None => thread::sleep(Duration::from_millis(5)),
        }
    };
    let out = reader.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        Err(err.trim().to_string())
    }
}

fn pactl(args: &[&str]) -> Result<String, String> {
    run_with_timeout(Command::new("pactl").env("LC_ALL", "C").args(args), PACTL_TIMEOUT)
}

/// Sets sink-input volume through `pactl`, caching name → index. Indexes change
/// when PipeWire restarts, so a failed set re-resolves once.
#[derive(Default)]
pub struct Pactl {
    cache: HashMap<String, u32>,
}

impl Pactl {
    fn resolve(&mut self, name: &str) -> Result<u32, String> {
        if let Some(&i) = self.cache.get(name) {
            return Ok(i);
        }
        let list = pactl(&["list", "sink-inputs"])?;
        let i = find_sink_input(&list, name).ok_or_else(|| format!("no stream {name}"))?;
        self.cache.insert(name.to_string(), i);
        Ok(i)
    }

    fn try_set(&mut self, name: &str, percent: u8) -> Result<(), String> {
        let i = self.resolve(name)?.to_string();
        pactl(&["set-sink-input-volume", &i, &format!("{percent}%")]).map(drop)
    }
}

impl VolumeSink for Pactl {
    fn set(&mut self, target: &str, percent: u8) -> Result<(), String> {
        self.try_set(target, percent).or_else(|_| {
            self.cache.remove(target);
            self.try_set(target, percent)
        })
    }
}

pub struct Mixer<V: VolumeSink> {
    sink: V,
    debounce: Duration,
    pending: Option<((u8, u8), Instant)>,
    applied: Option<(u8, u8)>,
}

impl<V: VolumeSink> Mixer<V> {
    pub fn new(sink: V, debounce: Duration) -> Self {
        Self { sink, debounce, pending: None, applied: None }
    }

    /// Record the latest wheel value; applied by a later `flush`.
    pub fn submit(&mut self, game: u8, chat: u8, now: Instant) {
        self.pending = Some(((game, chat), now));
    }

    pub fn flush(&mut self, now: Instant) {
        let Some((mix, at)) = self.pending else { return };
        if now.duration_since(at) < self.debounce {
            return;
        }
        self.pending = None;
        if self.applied == Some(mix) {
            return;
        }
        let (game, chat) = mix;
        let r1 = self.sink.set(GAME_TARGET, game);
        let r2 = self.sink.set(CHAT_TARGET, chat);
        match r1.and(r2) {
            Ok(()) => self.applied = Some(mix),
            Err(e) => eprintln!("arctisd: set volume: {e}"),
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.map(|(_, at)| at + self.debounce)
    }

    #[cfg(test)]
    fn sink(&self) -> &V {
        &self.sink
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Rec {
        calls: Vec<(String, u8)>,
        fail: bool,
    }
    impl VolumeSink for Rec {
        fn set(&mut self, sink: &str, percent: u8) -> Result<(), String> {
            self.calls.push((sink.to_string(), percent));
            if self.fail { Err("boom".into()) } else { Ok(()) }
        }
    }

    const D: Duration = Duration::from_millis(50);

    #[test]
    fn burst_applies_last_only() {
        let t0 = Instant::now();
        let mut m = Mixer::new(Rec::default(), D);
        m.submit(10, 90, t0);
        m.submit(40, 60, t0);
        m.submit(70, 30, t0);
        m.flush(t0 + Duration::from_millis(49));
        assert!(m.sink().calls.is_empty());
        m.flush(t0 + D);
        assert_eq!(
            m.sink().calls,
            vec![(GAME_TARGET.to_string(), 70), (CHAT_TARGET.to_string(), 30)]
        );
    }

    #[test]
    fn same_value_not_reapplied() {
        let t0 = Instant::now();
        let mut m = Mixer::new(Rec::default(), D);
        m.submit(70, 30, t0);
        m.flush(t0 + D);
        m.submit(70, 30, t0 + D);
        m.flush(t0 + D * 3);
        assert_eq!(m.sink().calls.len(), 2);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn error_does_not_panic() {
        let t0 = Instant::now();
        let mut m = Mixer::new(Rec { fail: true, ..Default::default() }, D);
        m.submit(70, 30, t0);
        m.flush(t0 + D);
        m.flush(t0 + D * 2);
        assert_eq!(m.sink().calls.len(), 2);
        m.submit(50, 50, t0 + D * 2);
        m.flush(t0 + D * 3);
        assert_eq!(m.sink().calls.len(), 4);
    }

    #[test]
    fn deadline_tracks_pending() {
        let t0 = Instant::now();
        let mut m = Mixer::new(Rec::default(), D);
        assert_eq!(m.next_deadline(), None);
        m.submit(1, 2, t0);
        assert_eq!(m.next_deadline(), Some(t0 + D));
    }
}

#[cfg(test)]
mod pactl_tests {
    use super::*;

    const LIST: &str = "Sink Input #53\n\tDriver: PipeWire\n\tProperties:\n\t\tnode.name = \"arctis_game.out\"\n\
Sink Input #55\n\tProperties:\n\t\tnode.name = \"arctis_chat.out\"\n\
Sink Input #1051\n\tProperties:\n\t\tnode.name = \"WEBRTC VoiceEngine\"\n";

    #[test]
    fn finds_sink_input_index_by_node_name() {
        assert_eq!(find_sink_input(LIST, "arctis_game.out"), Some(53));
        assert_eq!(find_sink_input(LIST, "arctis_chat.out"), Some(55));
        assert_eq!(find_sink_input(LIST, "arctis_media.out"), None);
        assert_eq!(find_sink_input(LIST, "arctis_game"), None);
    }

    #[test]
    fn targets_are_loopback_streams_not_sinks() {
        // Changing the sinks' own volume makes KDE pop its volume OSD.
        assert_eq!(GAME_TARGET, "arctis_game.out");
        assert_eq!(CHAT_TARGET, "arctis_chat.out");
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn hung_command_is_killed() {
        let t = Instant::now();
        let r = run_with_timeout(Command::new("sleep").arg("5"), Duration::from_millis(200));
        assert!(r.is_err());
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn fast_command_returns_stdout() {
        let mut c = Command::new("echo");
        c.arg("hi");
        assert_eq!(run_with_timeout(&mut c, Duration::from_secs(2)).unwrap(), "hi\n");
    }
}
