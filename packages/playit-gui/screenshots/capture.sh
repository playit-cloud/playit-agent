#!/usr/bin/env bash
# Captures the GUI in every state the mock daemon can simulate, on a headless X server.
#
# usage: capture.sh [out_dir]
#
# Environment:
#   SCREENSHOT_DISPLAY  X display to use or start (default :99)
#   SKIP_BUILD=1        use the existing debug binaries instead of running cargo build
#   SKIP_WAYLAND=1      skip the Wayland title-bar shot even when weston is installed
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
out="$(mkdir -p "${1:-$repo/target/screenshots}" && cd "${1:-$repo/target/screenshots}" && pwd)"
display="${SCREENSHOT_DISPLAY:-:99}"

for tool in Xvfb openbox xdotool xwininfo import python3; do
    command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done

if [ -z "${SKIP_BUILD:-}" ]; then
    (cd "$repo" && cargo build -p playit-gui --example mock_daemon --bin playit-gui)
fi
target="$(cd "$repo" && cargo metadata --format-version 1 --no-deps | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
gui="$target/debug/playit-gui"
mock="$target/debug/examples/mock_daemon"

work="$(mktemp -d)"
pids=()
cleanup() {
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    rm -rf "$work"
}
trap cleanup EXIT

# Starts a background process, remembering it for cleanup; its PID lands in $last_pid.
spawn() {
    "$@" >>"$work/log" 2>&1 &
    pids+=("$!")
    last_pid=$!
}

export DISPLAY="$display"
if ! xdotool getdisplaygeometry >/dev/null 2>&1; then
    spawn Xvfb "$display" -screen 0 1600x1100x24
    sleep 1
    # Without a window manager the GUI's window stays black on bare Xvfb.
    spawn openbox
    sleep 1
fi

# The setup screen talks to the playit API to create a claim; this stub keeps it waiting.
spawn python3 -I "$here/api_stub.py" 8787
sleep 0.5

# shoot <scenario|none> <name> [wait_secs] [WxH]
shoot() {
    local scenario=$1 name=$2 wait=${3:-4} size=${4:-}
    local socket="$work/mock.sock" mock_pid="" gui_pid window
    rm -f "$socket"
    if [ "$scenario" != none ]; then
        spawn "$mock" "$socket" "$scenario"
        mock_pid=$last_pid
        sleep 0.5
    fi
    # Park the pointer in a corner so no hover state shows.
    xdotool mousemove 1590 1090
    API_BASE=http://127.0.0.1:8787 PLAYIT_LOG=warn spawn "$gui" --socket-path "$socket"
    gui_pid=$last_pid
    window="$(timeout 15 xdotool search --sync --onlyvisible --name '^playit$' | head -1)"
    [ -n "$size" ] && xdotool windowsize "$window" "${size%x*}" "${size#*x}"
    sleep "$wait"
    import -window "$window" "$out/$name.png"
    kill "$gui_pid" ${mock_pid:+"$mock_pid"} 2>/dev/null || true
    wait "$gui_pid" ${mock_pid:+"$mock_pid"} 2>/dev/null || true
    echo "captured $out/$name.png"
}

for scenario in running empty setup starting error invalid-secret over-limit; do
    shoot "$scenario" "$scenario" 5
done
shoot running running-small-640x480 4 640x480
shoot none unreachable 3

# On Wayland the GUI draws its own title bar. A nested weston shows it.
if [ -z "${SKIP_WAYLAND:-}" ] && command -v weston >/dev/null; then
    runtime="$work/xdg"
    mkdir -m 700 "$runtime"
    XDG_RUNTIME_DIR="$runtime" spawn weston --backend=x11 --width=1280 --height=900 --socket=playit-shots
    sleep 4
    spawn "$mock" "$work/mock.sock" running
    sleep 0.5
    env -u DISPLAY XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=playit-shots PLAYIT_LOG=warn \
        "$gui" --socket-path "$work/mock.sock" >>"$work/log" 2>&1 &
    pids+=("$!")
    sleep 6
    # xdotool's name search misses weston's window, so find it in the window tree.
    weston_window="$(xwininfo -root -tree | awk '/"Weston Compositor - screen0"/ {print $1; exit}')"
    [ -n "$weston_window" ] || { echo "weston's window did not appear" >&2; exit 1; }
    xdotool windowraise "$weston_window"
    sleep 2
    import -window "$weston_window" "$out/wayland-titlebar.png"
    echo "captured $out/wayland-titlebar.png"
fi
