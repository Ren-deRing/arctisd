//! Gather what we know about the app behind a PipeWire/Pulse sink-input.

use crate::classify::AppInfo;
use crate::gamedb::candidates;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkInput {
    pub index: u32,
    pub sink: u32,
    pub props: HashMap<String, String>,
}

/// Parse `LC_ALL=C pactl list sink-inputs`.
pub fn parse_sink_inputs(list: &str) -> Vec<SinkInput> {
    let mut out: Vec<SinkInput> = Vec::new();
    for line in list.lines() {
        if let Some(n) = line.strip_prefix("Sink Input #") {
            if let Ok(index) = n.trim().parse() {
                out.push(SinkInput { index, sink: u32::MAX, props: HashMap::new() });
            }
            continue;
        }
        let Some(cur) = out.last_mut() else { continue };
        let t = line.trim();
        if let Some(n) = t.strip_prefix("Sink: ") {
            cur.sink = n.trim().parse().unwrap_or(u32::MAX);
        } else if let Some((k, v)) = t.split_once(" = \"") {
            cur.props.insert(k.to_string(), v.trim_end_matches('"').to_string());
        }
    }
    out
}

fn is_wine(bin: &str) -> bool {
    bin.starts_with("wine") || bin.ends_with("-preloader")
}

/// Stable per-app key: the process binary, or for Wine the Windows exe name.
pub fn app_key(props: &HashMap<String, String>) -> Option<String> {
    let bin = props.get("application.process.binary").filter(|b| !b.is_empty() && !is_wine(b));
    bin.or_else(|| props.get("application.name")).filter(|s| !s.is_empty()).cloned()
}

/// Started by Steam or umu (`SteamGameId` / `GAMEID` set, non-empty, not `0`).
pub fn launcher_game(environ: &[u8]) -> bool {
    environ.split(|&b| b == 0).any(|kv| {
        let kv = String::from_utf8_lossy(kv);
        match kv.split_once('=') {
            Some(("SteamGameId" | "GAMEID", v)) => !v.is_empty() && v != "0",
            _ => false,
        }
    })
}

/// `[Application] name=` from `/.flatpak-info`.
pub fn flatpak_id(info: &str) -> Option<String> {
    let mut in_app = false;
    for line in info.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_app = l == "[Application]";
        } else if in_app && let Some(v) = l.strip_prefix("name=") {
            return Some(v.to_string());
        }
    }
    None
}

fn field<'a>(desktop: &'a str, key: &str) -> Option<&'a str> {
    desktop.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
}

fn categories_of(desktop: &str) -> Vec<String> {
    field(desktop, "Categories")
        .map(|c| c.split(';').filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn matches_binary(desktop: &str, binary: &str) -> bool {
    let exec = field(desktop, "Exec")
        .and_then(|e| e.split_whitespace().next())
        .map(|t| t.rsplit('/').next().unwrap_or(t));
    let wm = field(desktop, "StartupWMClass");
    [exec, wm].into_iter().flatten().any(|n| n.eq_ignore_ascii_case(binary))
}

/// Categories of the app's `.desktop` file: by Flatpak ID, else by Exec/StartupWMClass.
pub fn desktop_categories(dirs: &[PathBuf], flatpak: Option<&str>, binary: &str) -> Vec<String> {
    if let Some(id) = flatpak {
        for d in dirs {
            if let Ok(s) = std::fs::read_to_string(d.join(format!("{id}.desktop"))) {
                return categories_of(&s);
            }
        }
    }
    if binary.is_empty() {
        return Vec::new();
    }
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let mut files: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "desktop")).collect();
        files.sort();
        for f in files {
            if let Ok(s) = std::fs::read_to_string(&f)
                && matches_binary(&s, binary)
            {
                return categories_of(&s);
            }
        }
    }
    Vec::new()
}

fn desktop_dirs() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    vec![
        home.join(".local/share/applications"),
        home.join(".local/share/flatpak/exports/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from("/usr/share/applications"),
    ]
}

/// Everything `classify` needs. Unreadable `/proc` entries just contribute nothing.
pub fn gather(si: &SinkInput) -> Option<AppInfo> {
    let key = app_key(&si.props)?;
    let binary = si.props.get("application.process.binary").cloned().unwrap_or_default();
    let name = si.props.get("application.name").cloned().unwrap_or_default();
    let proc_dir = si
        .props
        .get("application.process.id")
        .and_then(|p| p.parse::<u32>().ok())
        .map(|pid| PathBuf::from(format!("/proc/{pid}")));
    let read = |rel: &str| proc_dir.as_deref().and_then(|d: &Path| std::fs::read(d.join(rel)).ok());

    let launcher = read("environ").is_some_and(|e| launcher_game(&e));
    let argv0 = read("cmdline")
        .and_then(|c| c.split(|&b| b == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()))
        .unwrap_or_default();
    let fp = read("root/.flatpak-info").and_then(|i| flatpak_id(&String::from_utf8_lossy(&i)));

    let mut names: Vec<String> = [argv0.as_str(), name.as_str(), binary.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .flat_map(|s| candidates(s))
        .collect();
    names.sort();
    names.dedup();

    let bin_for_desktop = if binary.is_empty() || is_wine(&binary) { &key } else { &binary };
    let categories = desktop_categories(&desktop_dirs(), fp.as_deref(), bin_for_desktop);
    Some(AppInfo { key, launcher_game: launcher, names, categories })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "Sink Input #12\n\tDriver: PipeWire\n\tSink: 56\n\tProperties:\n\t\tapplication.name = \"WEBRTC VoiceEngine\"\n\t\tapplication.process.binary = \"Discord\"\n\t\tapplication.process.id = \"22445\"\n\
Sink Input #13\n\tSink: 58\n\tProperties:\n\t\tnode.name = \"arctis_game.out\"\n";

    fn props(kv: &[(&str, &str)]) -> HashMap<String, String> {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn parse_sink_inputs_basic() {
        let v = parse_sink_inputs(LIST);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].index, v[0].sink), (12, 56));
        assert_eq!(v[0].props["application.process.binary"], "Discord");
        assert_eq!(v[0].props["application.process.id"], "22445");
        assert_eq!((v[1].index, v[1].sink), (13, 58));
        assert_eq!(v[1].props["node.name"], "arctis_game.out");
    }

    #[test]
    fn app_key_prefers_binary() {
        let p = props(&[("application.process.binary", "Discord"), ("application.name", "WEBRTC VoiceEngine")]);
        assert_eq!(app_key(&p).as_deref(), Some("Discord"));
    }

    #[test]
    fn app_key_wine_uses_name() {
        let p = props(&[("application.process.binary", "wine64-preloader"), ("application.name", "BlueArchive.exe")]);
        assert_eq!(app_key(&p).as_deref(), Some("BlueArchive.exe"));
        let p = props(&[("application.process.binary", "wine"), ("application.name", "HYP.exe")]);
        assert_eq!(app_key(&p).as_deref(), Some("HYP.exe"));
        assert_eq!(app_key(&props(&[])), None);
    }

    #[test]
    fn app_key_webrtc_chrome() {
        let p = props(&[("application.process.binary", "chrome"), ("application.name", "WEBRTC VoiceEngine")]);
        assert_eq!(app_key(&p).as_deref(), Some("chrome"));
    }

    #[test]
    fn launcher_env() {
        assert!(launcher_game(b"HOME=/h\0SteamGameId=123\0"));
        assert!(!launcher_game(b"SteamGameId=0\0"));
        assert!(launcher_game(b"GAMEID=umu-1\0"));
        assert!(!launcher_game(b"GAMEID=\0X=1\0"));
        assert!(!launcher_game(b"HOME=/h\0"));
    }

    fn desktop_dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arctisd-desk-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for (f, body) in files {
            std::fs::write(d.join(f), body).unwrap();
        }
        d
    }

    #[test]
    fn desktop_by_exec() {
        let d = desktop_dir("exec", &[
            ("other.desktop", "[Desktop Entry]\nExec=/usr/bin/foo\nCategories=Game;\n"),
            ("discord.desktop", "[Desktop Entry]\nExec=/usr/bin/discord --url -- %u\nCategories=Network;InstantMessaging;\n"),
        ]);
        assert_eq!(desktop_categories(std::slice::from_ref(&d), None, "Discord"), vec!["Network", "InstantMessaging"]);
        assert!(desktop_categories(std::slice::from_ref(&d), None, "nothing").is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn desktop_by_wmclass() {
        let d = desktop_dir("wm", &[("x.desktop", "[Desktop Entry]\nExec=/opt/x/run.sh\nStartupWMClass=Spotify\nCategories=Audio;Music;\n")]);
        assert_eq!(desktop_categories(std::slice::from_ref(&d), None, "spotify"), vec!["Audio", "Music"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn desktop_by_flatpak_id() {
        let d = desktop_dir("fp", &[("com.google.Chrome.desktop", "[Desktop Entry]\nExec=/usr/bin/flatpak run com.google.Chrome\nCategories=Network;WebBrowser;\n")]);
        assert_eq!(desktop_categories(std::slice::from_ref(&d), Some("com.google.Chrome"), "chrome"), vec!["Network", "WebBrowser"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn flatpak_id_parse() {
        assert_eq!(flatpak_id("[Application]\nname=com.google.Chrome\nruntime=x\n").as_deref(), Some("com.google.Chrome"));
        assert_eq!(flatpak_id("[Runtime]\nname=org.x\n"), None);
    }

    #[test]
    fn gather_dead_pid() {
        let si = SinkInput {
            index: 1,
            sink: 2,
            props: props(&[("application.process.binary", "Foo"), ("application.process.id", "999999999")]),
        };
        let a = gather(&si).unwrap();
        assert_eq!(a.key, "Foo");
        assert!(!a.launcher_game);
        assert!(a.names.contains(&"foo".to_string()));
    }
}
