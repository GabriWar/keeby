# keeby

Lightweight Wayland on-screen overlay that shows what your keyboard and mouse are doing. Pure Rust, ~1 MB binary, no GTK, no Qt, no Electron.

Built for screencasts, livestreams, pair-debugging — anywhere you want viewers to actually see what you're pressing.

![keeby overview](assets/overview.png)

Live mouse direction indicator follows your motion:

![mouse closeup](assets/mouse.png)

## Features

- **Keystroke FIFO** — every press appears as a pill, oldest scrolls left, fades after a few seconds
- **Smart chord pills** — `Ctrl+Shift+A` shows up as one pill, not three. Modifier-only presses don't clutter the stream.
- **Live mouse widget** — cursor disc with a direction-arrow that tracks motion, three button cells that light on click, scroll chevrons that flash on the wheel
- **Hot-plug aware** — `inotify` watches `/dev/input`, so unplugging/replugging a USB keyboard, switching a KVM, or letting a Bluetooth keeb wake up just works without a restart
- **Pick your devices** — whitelist keyboards or mice by name substring; useful when a Bluetooth headset advertises media keys you don't want polluting the stream
- **Privacy panic key** — single tap (default `Pause`) freezes input capture and wipes the visible state; tap again to resume
- **Two independent layer surfaces** — keys go where you want, mouse widget goes where you want, separate anchor + margins
- **Custom font** — point at any `.ttf` / `.otf` you have installed; defaults to Cozette
- **Theme via plain hex** — paste your `pywal` / `hellwal` / `wallust` colors straight into the config
- **Per-monitor pinning** — lock to `eDP-1` or `HDMI-A-1` instead of "wherever was focused at launch"
- **Wayland-native** — uses `wlr-layer-shell`, click-through input region, never steals focus
- **Cheap** — ~1 MB binary, < 10 MB RSS, integer-math glyph blit, vsync-throttled redraws

## Install

### Arch (build from source)

```fish
git clone https://github.com/GabriWar/keeby.git
cd keeby
cargo build --release
install -Dm755 target/release/keeby ~/.local/bin/keeby
```

### Permissions

`keeby` reads `/dev/input/event*` directly via `evdev` (Wayland security forbids cross-client input sniffing, so there's no other way). Add yourself to the `input` group once:

```fish
sudo gpasswd -a $USER input
```

Log out and back in for it to take effect. Or for a non-persistent test:

```fish
sudo setfacl -m u:$USER:r /dev/input/event*
```

### Fonts

Defaults to [Cozette](https://github.com/slavfox/Cozette), falls back through JetBrains Mono and Noto. Install Cozette:

```fish
yay -S cozette-ttf
```

Or point `font = /path/to/anything.ttf` in the config.

## Usage

```fish
keeby            # run the overlay (default)
keeby --list     # list every detected keyboard, mouse, and monitor — copy these into config
keeby --help
```

Toggle from a window manager keybind (Hyprland example):

```ini
bind = SUPER+SHIFT, F5, exec, pkill -x keeby || keeby &
```

### Picking specific devices

By default keeby grabs every keyboard and every mouse it detects. If a stray device leaks into the stream (a Bluetooth headset advertising AVRCP "media keys", a fingerprint reader, a virtual KVM dongle, etc), run `keeby --list` to see the names, then whitelist what you actually want:

```ini
keyboards = AT Translated, Corne     # only my laptop keeb + my Corne, ignore TOZO headphones
mice      = Logitech                  # only the USB mouse, ignore the touchpad
```

Substrings are matched case-insensitively. Empty (or absent) means "accept all".

## Configuration

First launch writes a starter config to `~/.config/keeby/config.conf`. Every key is optional — defaults are sane.

```ini
# font
font = /usr/share/fonts/TTF/CozetteVector.ttf
size = 26
margin_bottom = 80
life = 3.5
fade = 1.0

# colors (hex #rrggbb or #rrggbbaa)
bg_color       = #21222cd9
accent_color   = #5c52a0eb
text_color     = #ffffffff
text_dim_color = #d2d6e6ff

# mouse widget
mouse_enabled       = true
mouse_size          = 110
mouse_anchor        = bottom-left
mouse_margin_x      = 30
mouse_margin_y      = 80
mouse_orientation   = horizontal
mouse_show_arrow    = true
mouse_show_buttons  = true
mouse_show_scroll   = true
mouse_arrow_color   = #ffc750f2

# pin to a specific output (or "auto")
monitor = eDP-1

# device whitelist — substrings, case-insensitive, comma-separated
# (run `keeby --list` to see candidates)
keyboards =
mice      =

# privacy panic key — single tap freezes capture, second tap resumes
# accepts F1..F12, Pause, ScrollLock, Print, Insert, NumLock, Menu, CapsLock,
# any single letter or digit. set `none` to disable.
privacy_key = Pause
```

`mouse_anchor` accepts: `top-left`, `top`, `top-right`, `left`, `center`, `right`, `bottom-left`, `bottom`, `bottom-right`.

## How it works

- One Wayland connection, two `wlr-layer-shell` surfaces in the `overlay` layer
- Click-through via empty `wl_region` as input region — never blocks anything underneath
- Drawing: `tiny-skia` rasterizes shapes, `fontdue` rasterizes glyphs, results memcpy'd directly into a `wl_shm` buffer with `Abgr8888` (no byte-swap needed on little-endian)
- Mouse redraws are gated by frame-callbacks: 1000 Hz mouse polling rate becomes ~vsync-rate redraws, not 1000 redraws/sec
- Input thread per `/dev/input/event*` device, classified by capability (mouse vs keyboard) and filtered by name
- Hot-plug: `inotify` on `/dev/input` watches `CREATE` + `ATTRIB`. New device → new reader thread. Old reader exits cleanly on EOF when its fd dies.

## Compositor support

Tested on Hyprland. Anything implementing `wlr-layer-shell-unstable-v1` should work (Sway, river, Niri, Wayfire, etc).

GNOME and KDE Plasma do not implement layer-shell — won't work there.

## Build deps

- Rust (stable)
- A working Wayland session
- Membership in the `input` group

That's it. No native libs to link, no font config to wire up.

## Why "keeby"

Because it shows your keys and it's a sloppy diminutive. That's it. Don't read into it.

## License

MIT
