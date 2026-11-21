//! hidraw discovery and I/O.

use crate::protocol::{self, Event};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

pub const VID: u16 = 0x1038;
pub const PID: u16 = 0x2253;
pub const CMD_IFACE: u8 = 3;

/// Find `/dev/hidrawN` whose sysfs device sits on USB interface `iface` of `vid:pid`.
pub fn find_hidraw(sys_root: &Path, vid: u16, pid: u16, iface: u8) -> Option<PathBuf> {
    let id = format!(":{vid:04X}:{pid:04X}.");
    let iface_seg = format!(":1.{iface}");
    let mut found: Vec<String> = std::fs::read_dir(sys_root.join("class/hidraw"))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let real = std::fs::canonicalize(e.path().join("device")).ok()?;
            let hid_dev = real.file_name()?.to_str()?;
            let parent = real.parent()?.file_name()?.to_str()?;
            (hid_dev.contains(&id) && parent.ends_with(&iface_seg))
                .then(|| e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    found.sort();
    found.first().map(|n| PathBuf::from("/dev").join(n))
}

fn find(iface: u8) -> Option<PathBuf> {
    find_hidraw(Path::new("/sys"), VID, PID, iface)
}

/// Reader thread: never exits; re-discovers the device after errors/unplug.
pub fn spawn_reader(iface: u8, tx: Sender<Event>) {
    thread::spawn(move || loop {
        if let Some(path) = find(iface) {
            match OpenOptions::new().read(true).open(&path) {
                Ok(mut f) => {
                    // Device just (re)appeared: ask for a fresh status.
                    let _ = request_status();
                    let mut buf = [0u8; 64];
                    loop {
                        match f.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Some(ev) = protocol::parse(&buf[..n])
                                    && tx.send(ev).is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => eprintln!("arctisd: open {}: {e}", path.display()),
            }
            // Dongle gone (or unreadable): don't leave a stale "connected" on screen.
            if tx.send(Event::Connected(false)).is_err() {
                return;
            }
        }
        thread::sleep(Duration::from_secs(1));
    });
}

/// Write the status request to the command interface; the reply arrives on a reader.
pub fn request_status() -> io::Result<()> {
    let path = find(CMD_IFACE).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no dongle"))?;
    OpenOptions::new().write(true).open(path)?.write_all(&protocol::STATUS_REQUEST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fake_sys() -> PathBuf {
        let root = std::env::temp_dir().join(format!("arctisd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dev = root.join("devices/pci0000:00/usb1/1-3");
        for (n, iface) in [(4, 3), (5, 5)] {
            let target = dev.join(format!("1-3:1.{iface}/0003:1038:2253.000{n}"));
            std::fs::create_dir_all(&target).unwrap();
            let link_dir = root.join(format!("class/hidraw/hidraw{n}"));
            std::fs::create_dir_all(&link_dir).unwrap();
            symlink(&target, link_dir.join("device")).unwrap();
        }
        root
    }

    #[test]
    fn find_hidraw_matches_interface() {
        let root = fake_sys();
        assert_eq!(find_hidraw(&root, 0x1038, 0x2253, 3), Some(PathBuf::from("/dev/hidraw4")));
        assert_eq!(find_hidraw(&root, 0x1038, 0x2253, 5), Some(PathBuf::from("/dev/hidraw5")));
        assert_eq!(find_hidraw(&root, 0x1038, 0x2232, 3), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
