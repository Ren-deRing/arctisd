//! Discord "detectable" game DB, kept on disk as a sorted list and searched in place.

use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const DB_URL: &str = "https://raw.githubusercontent.com/OpenAsar/arRPC/main/src/process/detectable.json";

/// Executable basenames shorter than this are too generic to trust.
const MIN_BASENAME: usize = 4;
/// Longest path suffix tried for a lookup (`a/b/c/d.exe`).
const MAX_DEPTH: usize = 4;

fn normalize(name: &str) -> String {
    name.to_lowercase().replace('\\', "/")
}

pub fn db_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    base.join("arctisd/games.txt")
}

/// Sorted, de-duplicated, lower-case executable names of non-launcher entries.
pub fn build(json: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let apps = v.as_array().ok_or("detectable.json is not an array")?;
    let mut out: Vec<String> = apps
        .iter()
        .filter_map(|a| a.get("executables")?.as_array())
        .flatten()
        .filter(|e| e.get("is_launcher").and_then(|l| l.as_bool()) != Some(true))
        .filter_map(|e| e.get("name")?.as_str())
        .map(normalize)
        .filter(|n| n.rsplit('/').next().is_some_and(|b| b.len() >= MIN_BASENAME))
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// Lookup keys for a process name or (Windows/Unix) path: trailing path suffixes.
pub fn candidates(path_or_name: &str) -> Vec<String> {
    let n = normalize(path_or_name);
    let n = match n.as_bytes() {
        [d, b':', ..] if d.is_ascii_alphabetic() => &n[2..],
        _ => &n[..],
    };
    let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty()).collect();
    (1..=parts.len().min(MAX_DEPTH)).map(|k| parts[parts.len() - k..].join("/")).collect()
}

/// First full line starting at byte offset >= `off`, with its start offset.
fn line_at_or_after(r: &mut BufReader<File>, off: u64) -> Option<(u64, String)> {
    let start = if off == 0 {
        r.seek(SeekFrom::Start(0)).ok()?;
        0
    } else {
        r.seek(SeekFrom::Start(off - 1)).ok()?;
        let mut skip = Vec::new();
        off - 1 + r.read_until(b'\n', &mut skip).ok()? as u64
    };
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    Some((start, line.trim_end_matches('\n').to_string()))
}

/// Binary search a sorted, newline-separated file without loading it.
pub fn contains(path: &Path, key: &str) -> bool {
    let Ok(f) = File::open(path) else { return false };
    let Ok(len) = f.metadata().map(|m| m.len()) else { return false };
    let mut r = BufReader::with_capacity(512, f);
    let (mut lo, mut hi) = (0u64, len);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match line_at_or_after(&mut r, mid) {
            Some((start, line)) if start < hi => match line.as_str().cmp(key) {
                Ordering::Equal => return true,
                Ordering::Less => lo = start + line.len() as u64 + 1,
                Ordering::Greater => hi = mid,
            },
            _ => hi = mid,
        }
    }
    false
}

pub fn in_db(names: &[String]) -> bool {
    let p = db_path();
    names.iter().any(|n| contains(&p, n))
}

/// Download detectable.json and rewrite the sorted list. Returns the entry count.
pub fn update_db() -> Result<usize, String> {
    let json = crate::command::run_with_timeout(
        Command::new("curl").args(["-fsSL", "--max-time", "30", DB_URL]),
        Duration::from_secs(35),
    )?;
    let list = build(&json)?;
    let path = db_path();
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, list.join("\n") + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(list.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("arctisd-gamedb-{}-{name}", std::process::id()))
    }

    #[test]
    fn build_filters_and_sorts() {
        let json = r#"[
            {"executables":[{"name":"GenshinImpact.exe","os":"win32","is_launcher":false},
                            {"name":"Launcher.exe","is_launcher":true},
                            {"name":"BlueArchive\\BlueArchive.exe"},
                            {"name":"ab"}]},
            {"name":"NoExe"},
            {"executables":[{"name":"genshinimpact.exe","os":"win32"}]}
        ]"#;
        assert_eq!(build(json).unwrap(), vec!["bluearchive/bluearchive.exe", "genshinimpact.exe"]);
    }

    #[test]
    fn candidates_from_wine_path() {
        assert_eq!(
            candidates(r"Z:\G\BlueArchive\BlueArchive.exe"),
            vec!["bluearchive.exe", "bluearchive/bluearchive.exe", "g/bluearchive/bluearchive.exe"]
        );
        assert_eq!(candidates("Discord"), vec!["discord"]);
    }

    #[test]
    fn contains_binary_search() {
        let p = tmp("search");
        let mut f = std::fs::File::create(&p).unwrap();
        for i in 0..1000 {
            writeln!(f, "g{i:04}.exe").unwrap();
        }
        drop(f);
        for k in ["g0000.exe", "g0500.exe", "g0999.exe", "g0333.exe"] {
            assert!(contains(&p, k), "{k}");
        }
        for k in ["g1000.exe", "a.exe", "g0500.ex", "zzz"] {
            assert!(!contains(&p, k), "{k}");
        }
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn contains_missing_file_is_false() {
        assert!(!contains(&tmp("nope"), "anything"));
    }
}
