//! Move new audio streams to Game/Chat (Media is the default sink) and learn
//! from the user's own moves.

use crate::appinfo::{self, SinkInput};
use crate::classify::{AppInfo, Channel, classify};
use crate::gamedb;
use crate::mixer::run_with_timeout;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const OWN_PREFIX: &str = "arctis_";
/// pactl's sink index for a stream that is not linked to any sink yet.
const UNLINKED: u32 = u32::MAX;
const PACTL_TIMEOUT: Duration = Duration::from_secs(1);
/// Sink changes this soon after a stream is linked are WirePlumber relinking, not the user.
const LINK_GRACE: Duration = Duration::from_secs(1);
/// After the default sink changes, KDE/WirePlumber move streams in bulk: don't learn.
const DEFAULT_CHANGE_QUIET: Duration = Duration::from_secs(2);

/// App key → channel the user chose by moving the stream themselves.
pub struct Overrides {
    map: HashMap<String, Channel>,
    path: PathBuf,
}

impl Overrides {
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
        base.join("arctisd/overrides")
    }

    pub fn load(path: &Path) -> Self {
        let map = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (k, c) = l.rsplit_once('\t')?;
                Some((k.to_string(), Channel::parse(c)?))
            })
            .collect();
        Self { map, path: path.to_path_buf() }
    }

    pub fn get(&self, key: &str) -> Option<Channel> {
        self.map.get(key).copied()
    }

    pub fn set(&mut self, key: &str, ch: Channel) {
        self.map.insert(key.to_string(), ch);
        let mut lines: Vec<String> = self.map.iter().map(|(k, c)| format!("{k}\t{}\n", c.as_str())).collect();
        lines.sort();
        let write = || -> std::io::Result<()> {
            if let Some(d) = self.path.parent() {
                std::fs::create_dir_all(d)?;
            }
            let tmp = self.path.with_extension("tmp");
            std::fs::write(&tmp, lines.concat())?;
            std::fs::rename(&tmp, &self.path)
        };
        if let Err(e) = write() {
            eprintln!("arctisd: save overrides: {e}");
        }
    }
}

pub trait PulseOps {
    fn list(&mut self) -> Result<String, String>;
    fn move_input(&mut self, idx: u32, sink: &str) -> Result<(), String>;
    fn sink_index(&mut self, name: &str) -> Option<u32>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    New,
    Change,
    Remove,
    /// Default sink switched or a sink added/removed: bulk relinks follow.
    Server,
}

/// `Event 'new' on sink-input #12` (LC_ALL=C `pactl subscribe`).
pub fn parse_event(line: &str) -> Option<(Kind, u32)> {
    // Default sink switched, or a sink appeared/vanished (e.g. WirePlumber
    // restart): streams get relinked in bulk right after these.
    if line.starts_with("Event 'change' on server")
        || line.starts_with("Event 'new' on sink #")
        || line.starts_with("Event 'remove' on sink #")
    {
        return Some((Kind::Server, 0));
    }
    let rest = line.strip_prefix("Event '")?;
    let (kind, rest) = rest.split_once("' on sink-input #")?;
    let kind = match kind {
        "new" => Kind::New,
        "change" => Kind::Change,
        "remove" => Kind::Remove,
        _ => return None,
    };
    Some((kind, rest.trim().parse().ok()?))
}

struct Tracked {
    key: String,
    last_sink: u32,
    /// Sink we just moved it to; the change event for it is ours, not the user's.
    expect: Option<u32>,
    /// Decided channel not applied yet (stream was not linked when it appeared).
    pending: Option<Channel>,
    linked_at: Option<Instant>,
    /// Where we decided this stream belongs.
    channel: Option<Channel>,
}

type Gather = Box<dyn Fn(&SinkInput) -> Option<AppInfo>>;
type InDb = Box<dyn Fn(&[String]) -> bool>;

pub struct Router<P: PulseOps> {
    ops: P,
    overrides: Overrides,
    gather: Gather,
    in_db: InDb,
    tracked: HashMap<u32, Tracked>,
    ignored: HashSet<u32>,
    /// Automatic decision per app key (DB / .desktop lookups happen once).
    cache: HashMap<String, Channel>,
    quiet_until: Option<Instant>,
}

impl<P: PulseOps> Router<P> {
    pub fn new(ops: P, overrides: Overrides, gather: Gather, in_db: InDb) -> Self {
        Self { ops, overrides, gather, in_db, tracked: HashMap::new(), ignored: HashSet::new(), cache: HashMap::new(), quiet_until: None }
    }

    fn find(&mut self, idx: u32) -> Option<SinkInput> {
        let list = self.ops.list().map_err(|e| eprintln!("arctisd: list: {e}")).ok()?;
        appinfo::parse_sink_inputs(&list).into_iter().find(|s| s.index == idx)
    }

    fn channel_of_sink(&mut self, sink: u32) -> Option<Channel> {
        [Channel::Game, Channel::Chat, Channel::Media]
            .into_iter()
            .find(|c| self.ops.sink_index(c.sink()) == Some(sink))
    }

    pub fn on_server_change(&mut self, now: Instant) {
        self.quiet_until = Some(now + DEFAULT_CHANGE_QUIET);
    }

    pub fn on_new(&mut self, idx: u32, now: Instant) {
        if self.ignored.contains(&idx) {
            return;
        }
        let Some(si) = self.find(idx) else { return };
        if si.props.get("node.name").is_some_and(|n| n.starts_with(OWN_PREFIX)) {
            self.ignored.insert(idx);
            return;
        }
        let Some(info) = (self.gather)(&si) else { return };
        let target = self.decide(&info);
        let linked_at = (si.sink != UNLINKED).then_some(now);
        let mut t = Tracked { key: info.key, last_sink: si.sink, expect: None, pending: target, linked_at, channel: target };
        // Not linked yet: moving now is unreliable, do it on the first change.
        if si.sink != UNLINKED {
            self.apply(idx, &mut t);
        }
        self.tracked.insert(idx, t);
    }

    /// Where this app belongs (Media when nothing says otherwise).
    fn decide(&mut self, info: &AppInfo) -> Option<Channel> {
        if let Some(c) = self.overrides.get(&info.key) {
            return Some(c);
        }
        // Launcher env is per process, so it is never cached per app key.
        if info.launcher_game {
            return Some(Channel::Game);
        }
        let c = match self.cache.get(&info.key) {
            Some(&c) => c,
            None => {
                let c = classify(info, None, &self.in_db);
                self.cache.insert(info.key.clone(), c);
                c
            }
        };
        Some(c)
    }

    fn apply(&mut self, idx: u32, t: &mut Tracked) {
        let Some(ch) = t.pending.take() else { return };
        // Only re-route between our own inputs. WirePlumber may restore an app
        // onto the wrong one; a stream on a real device was put there on purpose.
        if self.channel_of_sink(t.last_sink).is_none() {
            return;
        }
        let Some(target) = self.ops.sink_index(ch.sink()) else { return };
        if target == t.last_sink {
            return;
        }
        // Expect the move even if pactl reports failure: it may still land.
        t.expect = Some(target);
        if let Err(e) = self.ops.move_input(idx, ch.sink()) {
            eprintln!("arctisd: move #{idx}: {e}");
        }
    }

    pub fn on_change(&mut self, idx: u32, now: Instant) {
        if self.ignored.contains(&idx) {
            return;
        }
        let Some(mut t) = self.tracked.remove(&idx) else { return self.on_new(idx, now) };
        let Some(si) = self.find(idx) else {
            self.tracked.insert(idx, t);
            return;
        };
        if si.sink != t.last_sink && si.sink != UNLINKED {
            let was_unlinked = t.last_sink == UNLINKED;
            let ours = t.expect == Some(si.sink);
            let settling = t.linked_at.is_some_and(|l| now.duration_since(l) < LINK_GRACE);
            let quiet = self.quiet_until.is_some_and(|q| now < q);
            if !ours && !was_unlinked {
                if settling || quiet {
                    // Session-manager relink, not the user: put it back.
                    t.pending = t.channel;
                } else if let Some(ch) = self.channel_of_sink(si.sink) {
                    self.overrides.set(&t.key, ch);
                    t.channel = Some(ch);
                }
            }
            t.last_sink = si.sink;
            t.expect = None;
            if was_unlinked {
                t.linked_at = Some(now);
            }
            self.apply(idx, &mut t);
        }
        self.tracked.insert(idx, t);
    }

    pub fn on_remove(&mut self, idx: u32) {
        self.tracked.remove(&idx);
        self.ignored.remove(&idx);
    }

    fn scan_existing(&mut self) {
        let Ok(list) = self.ops.list() else { return };
        for si in appinfo::parse_sink_inputs(&list) {
            self.on_new(si.index, Instant::now());
        }
    }
}

struct PactlOps;

impl PulseOps for PactlOps {
    fn list(&mut self) -> Result<String, String> {
        run_with_timeout(Command::new("pactl").env("LC_ALL", "C").args(["list", "sink-inputs"]), PACTL_TIMEOUT)
    }
    fn move_input(&mut self, idx: u32, sink: &str) -> Result<(), String> {
        run_with_timeout(Command::new("pactl").args(["move-sink-input", &idx.to_string(), sink]), PACTL_TIMEOUT)
            .map(drop)
    }
    fn sink_index(&mut self, name: &str) -> Option<u32> {
        let out = run_with_timeout(Command::new("pactl").args(["list", "short", "sinks"]), PACTL_TIMEOUT).ok()?;
        out.lines().find_map(|l| {
            let mut f = l.split('\t');
            let idx = f.next()?.parse().ok()?;
            (f.next()? == name).then_some(idx)
        })
    }
}

/// Router thread: follows `pactl subscribe`, restarting it if it exits.
pub fn spawn_router() {
    thread::spawn(|| {
        let mut r = Router::new(
            PactlOps,
            Overrides::load(&Overrides::default_path()),
            Box::new(appinfo::gather),
            Box::new(gamedb::in_db),
        );
        loop {
            let child = Command::new("pactl")
                .env("LC_ALL", "C")
                .arg("subscribe")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            match child {
                Ok(mut c) => {
                    r.scan_existing();
                    let out = c.stdout.take().expect("piped");
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        match parse_event(&line) {
                            Some((Kind::New, i)) => r.on_new(i, Instant::now()),
                            Some((Kind::Change, i)) => r.on_change(i, Instant::now()),
                            Some((Kind::Server, _)) => r.on_server_change(Instant::now()),
                            Some((Kind::Remove, i)) => r.on_remove(i),
                            None => {}
                        }
                    }
                    let _ = c.kill();
                    let _ = c.wait();
                }
                Err(e) => eprintln!("arctisd: pactl subscribe: {e}"),
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::OnceLock;
    use std::time::Instant;

    fn at(secs_x10: u64) -> Instant {
        static BASE: OnceLock<Instant> = OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(secs_x10 * 100)
    }

    #[derive(Clone, Default)]
    struct Fake {
        list: Rc<RefCell<String>>,
        moves: Rc<RefCell<Vec<(u32, String)>>>,
        fail_moves: bool,
    }
    impl PulseOps for Fake {
        fn list(&mut self) -> Result<String, String> {
            Ok(self.list.borrow().clone())
        }
        fn move_input(&mut self, idx: u32, sink: &str) -> Result<(), String> {
            self.moves.borrow_mut().push((idx, sink.to_string()));
            if self.fail_moves { Err("not linked yet".into()) } else { Ok(()) }
        }
        fn sink_index(&mut self, name: &str) -> Option<u32> {
            match name {
                "arctis_game" => Some(54),
                "arctis_chat" => Some(56),
                "arctis_media" => Some(58),
                _ => None,
            }
        }
    }

    fn input(idx: u32, sink: u32, bin: &str) -> String {
        format!("Sink Input #{idx}\n\tSink: {sink}\n\tProperties:\n\t\tapplication.process.binary = \"{bin}\"\n")
    }

    fn opath(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("arctisd-ovr-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn router(f: &Fake, path: &Path, db_hit: bool) -> Router<Fake> {
        Router::new(
            f.clone(),
            Overrides::load(path),
            Box::new(|si: &SinkInput| {
                let key = si.props.get("application.process.binary")?.clone();
                let launcher_game = si.props.contains_key("steam");
                Some(AppInfo { key, launcher_game, ..Default::default() })
            }),
            Box::new(move |_: &[String]| db_hit),
        )
    }

    #[test]
    fn parse_event_lines() {
        assert_eq!(parse_event("Event 'new' on sink-input #12"), Some((Kind::New, 12)));
        assert_eq!(parse_event("Event 'change' on sink-input #7"), Some((Kind::Change, 7)));
        assert_eq!(parse_event("Event 'remove' on sink-input #7"), Some((Kind::Remove, 7)));
        assert_eq!(parse_event("Event 'new' on sink #3"), Some((Kind::Server, 0)));
        assert_eq!(parse_event("Event 'remove' on sink #3"), Some((Kind::Server, 0)));
        assert_eq!(parse_event("Event 'change' on sink #3"), None);
        assert_eq!(parse_event("Event 'change' on server #4294967295"), Some((Kind::Server, 0)));
        assert_eq!(parse_event("garbage"), None);
    }

    #[test]
    fn new_game_is_moved_and_user_move_learned() {
        let p = opath("flow");
        let f = Fake::default();
        *f.list.borrow_mut() = input(7, 58, "Discord");
        let mut r = router(&f, &p, true);
        r.on_new(7, at(0));
        assert_eq!(*f.moves.borrow(), vec![(7, "arctis_game".to_string())]);

        // Our own move shows up as a change: not a user override.
        *f.list.borrow_mut() = input(7, 54, "Discord");
        r.on_change(7, at(10));
        assert!(!p.exists() || std::fs::read_to_string(&p).unwrap().is_empty());

        // User drags it to Chat in KDE.
        *f.list.borrow_mut() = input(7, 56, "Discord");
        r.on_change(7, at(10));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "Discord\tchat\n");

        // Next launch: override wins over the DB hit.
        let f2 = Fake::default();
        *f2.list.borrow_mut() = input(9, 58, "Discord");
        let mut r2 = router(&f2, &p, true);
        r2.on_new(9, at(0));
        assert_eq!(*f2.moves.borrow(), vec![(9, "arctis_chat".to_string())]);
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn own_loopbacks_ignored() {
        let p = opath("own");
        let f = Fake::default();
        *f.list.borrow_mut() =
            "Sink Input #53\n\tSink: 85\n\tProperties:\n\t\tnode.name = \"arctis_game.out\"\n\t\tapplication.process.binary = \"pipewire\"\n".into();
        let mut r = router(&f, &p, true);
        r.on_new(53, at(0));
        let moved = f.list.borrow().replace("Sink: 85", "Sink: 54");
        *f.list.borrow_mut() = moved;
        r.on_change(53, at(10));
        assert!(f.moves.borrow().is_empty());
        assert!(!p.exists());
    }

    #[test]
    fn media_not_moved() {
        let p = opath("media");
        let f = Fake::default();
        *f.list.borrow_mut() = input(3, 58, "chrome");
        let mut r = router(&f, &p, false);
        r.on_new(3, at(0));
        assert!(f.moves.borrow().is_empty());
    }

    #[test]
    fn already_on_target_not_moved() {
        let p = opath("ontarget");
        let f = Fake::default();
        *f.list.borrow_mut() = input(4, 54, "game");
        let mut r = router(&f, &p, true);
        r.on_new(4, at(0));
        assert!(f.moves.borrow().is_empty());
    }

    #[test]
    fn unlinked_stream_moved_after_link() {
        let p = opath("unlinked");
        let f = Fake::default();
        *f.list.borrow_mut() = input(5, u32::MAX, "game");
        let mut r = router(&f, &p, true);
        r.on_new(5, at(0));
        assert!(f.moves.borrow().is_empty());
        *f.list.borrow_mut() = input(5, 58, "game");
        r.on_change(5, at(10));
        assert_eq!(*f.moves.borrow(), vec![(5, "arctis_game".to_string())]);
        *f.list.borrow_mut() = input(5, 54, "game");
        r.on_change(5, at(10));
        assert!(!p.exists());
    }

    #[test]
    fn failed_move_that_lands_later_is_not_learned() {
        let p = opath("failmove");
        let f = Fake { fail_moves: true, ..Default::default() };
        *f.list.borrow_mut() = input(6, 58, "game");
        let mut r = router(&f, &p, true);
        r.on_new(6, at(0));
        *f.list.borrow_mut() = input(6, 54, "game");
        r.on_change(6, at(10));
        assert!(!p.exists());
    }

    #[test]
    fn launcher_env_not_cached_per_key() {
        let p = opath("nocache");
        let f = Fake::default();
        *f.list.borrow_mut() = input(1, 58, "pacat") + "\t\tsteam = \"1\"\n";
        let mut r = router(&f, &p, false);
        r.on_new(1, at(0));
        *f.list.borrow_mut() = input(2, 58, "pacat");
        r.on_new(2, at(0));
        assert_eq!(*f.moves.borrow(), vec![(1, "arctis_game".to_string())]);
    }

    #[test]
    fn media_app_restored_elsewhere_moved_back() {
        let p = opath("restored");
        let f = Fake::default();
        *f.list.borrow_mut() = input(8, 54, "pacat");
        let mut r = router(&f, &p, false);
        r.on_new(8, at(0));
        assert_eq!(*f.moves.borrow(), vec![(8, "arctis_media".to_string())]);
    }

    #[test]
    fn stream_on_hardware_sink_left_alone() {
        let p = opath("hw");
        let f = Fake::default();
        *f.list.borrow_mut() = input(9, 85, "game");
        let mut r = router(&f, &p, true);
        r.on_new(9, at(0));
        assert!(f.moves.borrow().is_empty());
    }

    #[test]
    fn default_sink_change_not_learned() {
        let p = opath("defchange");
        let f = Fake::default();
        *f.list.borrow_mut() = input(11, 58, "firefox");
        let mut r = router(&f, &p, false);
        r.on_new(11, at(0));
        r.on_server_change(at(50));
        *f.list.borrow_mut() = input(11, 54, "firefox");
        r.on_change(11, at(60)); // 1s after the default changed
        assert!(!p.exists());
        // Long after: a real user move is learned again.
        *f.list.borrow_mut() = input(11, 56, "firefox");
        r.on_change(11, at(100));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "firefox\tchat\n");
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn relink_right_after_link_not_learned() {
        let p = opath("grace");
        let f = Fake::default();
        *f.list.borrow_mut() = input(12, u32::MAX, "mpv");
        let mut r = router(&f, &p, false);
        r.on_new(12, at(0));
        *f.list.borrow_mut() = input(12, 58, "mpv");
        r.on_change(12, at(20)); // linked at 2.0s
        *f.list.borrow_mut() = input(12, 54, "mpv");
        r.on_change(12, at(25)); // WirePlumber relinks 0.5s later
        assert!(!p.exists());
    }

    #[test]
    fn relink_during_quiet_is_rerouted_not_learned() {
        let p = opath("reroute");
        let f = Fake::default();
        *f.list.borrow_mut() = input(13, 58, "game");
        let mut r = router(&f, &p, true);
        r.on_new(13, at(0));
        *f.list.borrow_mut() = input(13, 54, "game");
        r.on_change(13, at(10)); // our move landed
        r.on_server_change(at(100)); // WirePlumber restarted, headset sink re-added
        *f.list.borrow_mut() = input(13, 58, "game");
        r.on_change(13, at(105)); // WirePlumber relinked it to the default
        assert!(!p.exists());
        assert_eq!(*f.moves.borrow(), vec![(13, "arctis_game".to_string()), (13, "arctis_game".to_string())]);
    }

    #[test]
    fn overrides_roundtrip_file() {
        let p = opath("rt");
        let mut o = Overrides::load(&p);
        o.set("a b", Channel::Media);
        o.set("Discord", Channel::Chat);
        let o2 = Overrides::load(&p);
        assert_eq!(o2.get("a b"), Some(Channel::Media));
        assert_eq!(o2.get("Discord"), Some(Channel::Chat));
        assert_eq!(o2.get("x"), None);
        std::fs::remove_file(&p).unwrap();
    }
}
