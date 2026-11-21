# arctisd

A small Linux daemon for the **SteelSeries Arctis Nova 5X** (USB dongle `1038:2253`),
replacing what SteelSeries GG / Sonar does on Windows:

- **Battery** in the system tray (StatusNotifierItem), with low-battery notifications at 20% and 10%.
- **ChatMix wheel**: the headset wheel balances two virtual inputs, *Arctis Game* and *Arctis Chat*.
  A third input, *Arctis Media*, is not affected by the wheel.
- **Automatic app routing**: new audio streams are sent to Game, Chat or Media.
  If you move an app yourself, that choice is remembered and takes priority.

It is written in Rust with no async runtime and uses about 5 MB RSS.
The audio mixing is done by PipeWire itself, so arctisd crashing or restarting never cuts your audio.

## How routing decides

1. Your own earlier move of that app (`~/.local/state/arctisd/overrides`, one `app<TAB>game|chat|media` per line).
2. Launched by Steam or umu (`SteamGameId` / `GAMEID` in the process environment) → **Game**.
3. Executable found in Discord's game-detection list (the `detectable.json` shipped by [arRPC](https://github.com/OpenAsar/arRPC)) → **Game**.
4. The app's `.desktop` categories: `InstantMessaging` / `Chat` → **Chat**, `Game` → **Game**.
5. Anything else → **Media** (the default sink).

The game list is stored as a sorted text file (`~/.cache/arctisd/games.txt`) and searched on disk with a binary search.
It is never loaded into memory. A weekly systemd timer refreshes it.

## Requirements

- PipeWire with `pipewire-pulse` and WirePlumber 0.5 (`pactl` must be available)
- A StatusNotifierItem tray (KDE Plasma works out of the box)
- `curl` (for `arctisd update-db`)
- Rust toolchain (to build)

## Install

```sh
cargo install --path .

# 1. Let your user access the dongle's hidraw nodes
sudo cp packaging/70-arctisd.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger

# 2. Game / Chat / Media virtual inputs, and let arctisd own per-app routing
mkdir -p ~/.config/pipewire/pipewire.conf.d ~/.config/wireplumber/wireplumber.conf.d
cp packaging/arctisd-sinks.conf ~/.config/pipewire/pipewire.conf.d/
cp packaging/arctisd-wireplumber.conf ~/.config/wireplumber/wireplumber.conf.d/arctisd.conf
systemctl --user restart pipewire pipewire-pulse wireplumber
pactl set-default-sink arctis_media

# 3. Game list and services
arctisd update-db
mkdir -p ~/.config/systemd/user
cp packaging/arctisd.service packaging/arctisd-update-db.service packaging/arctisd-update-db.timer ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now arctisd.service arctisd-update-db.timer
```

`packaging/arctisd-sinks.conf` targets the node
`alsa_output.usb-SteelSeries_SteelSeries_Arctis_Nova_5X-00.analog-stereo`.
Check yours with `pactl list short sinks`.

Note that `arctisd-wireplumber.conf` turns off WirePlumber's per-app output memory for all apps,
so that arctisd alone decides where each app goes.

## Uninstall

```sh
systemctl --user disable --now arctisd.service arctisd-update-db.timer
rm ~/.config/systemd/user/arctisd.service ~/.config/systemd/user/arctisd-update-db.service ~/.config/systemd/user/arctisd-update-db.timer
rm ~/.config/pipewire/pipewire.conf.d/arctisd-sinks.conf ~/.config/wireplumber/wireplumber.conf.d/arctisd.conf
sudo rm /etc/udev/rules.d/70-arctisd.rules
cargo uninstall arctisd
```

## Protocol

HID interface 3 takes commands. Sending `00 B0`, padded to 64 bytes, returns this status report:

```
b0 <conn: 03 on / 02 off> <?> <battery %> <charging: 01> <game 0-100> <chat 0-100>
```

Interface 5 pushes events:

| Event | Meaning |
|---|---|
| `45 <game> <chat>` | ChatMix wheel moved |
| `b7 <battery>` | Battery level changed |
| `bb <charging>` | Charging state changed |
| `b9 <conn>` | Headset connected or disconnected |

These details come from [HeadsetControl](https://github.com/Sapd/HeadsetControl) and
[Linux-Arctis-Manager](https://github.com/elegos/Linux-Arctis-Manager), and were checked against real hardware.

## License

[0BSD](LICENSE)
