# GUI screenshots

`capture.sh` builds the GUI and `examples/mock_daemon`, then captures one PNG per state on a headless X server:

| File | State |
|---|---|
| `running.png` | Running at the default size, with active, pending and disabled tunnels and an update notice |
| `empty.png` | Running with no tunnels, on a guest account |
| `running-small-640x480.png` | Running at the minimum window size |
| `setup.png` | First-time setup, waiting for the browser |
| `starting.png`, `error.png`, `invalid-secret.png`, `over-limit.png` | The agent's other lifecycle states |
| `unreachable.png` | No daemon on the socket |
| `wayland-titlebar.png` | Running inside a nested weston, with the title bar the GUI draws on Wayland |

```sh
packages/playit-gui/screenshots/capture.sh            # writes to target/screenshots
packages/playit-gui/screenshots/capture.sh /tmp/shots # or to a directory of your choice
```

## Requirements

Linux with:

- `Xvfb` and `openbox`: the X server and window manager. Without a window manager the window stays black.
- A Vulkan driver. GPUI renders with Vulkan; on a machine without a GPU, Mesa's software driver (lavapipe, in `mesa-vulkan-drivers`) works.
- `xdotool` and `xwininfo` to find and resize windows, and ImageMagick's `import` to capture them.
- `python3`, for the stand-in playit API that keeps the setup screen waiting (`api_stub.py`).
- `weston`, optional, for `wayland-titlebar.png`.

On Debian or Ubuntu:

```sh
sudo apt install xvfb openbox xdotool x11-utils imagemagick mesa-vulkan-drivers python3 weston
```

## Options

| Variable | Effect |
|---|---|
| `SCREENSHOT_DISPLAY` | X display to use, `:99` by default. The script starts Xvfb there unless a server is already running. |
| `SKIP_BUILD=1` | Use the existing debug binaries instead of running `cargo build`. |
| `SKIP_WAYLAND=1` | Skip the weston capture. |

The mock daemon's scenarios are listed in `examples/mock_daemon.rs`. To look at one by hand, run `cargo run -p playit-gui --example mock_daemon -- /tmp/mock.sock error` and point the GUI at it with `--socket-path /tmp/mock.sock`.
