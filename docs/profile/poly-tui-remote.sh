# =====================================================================
# poly — remote monitoring helpers (remote trader → local TUI)
#
# Source from your ~/.bashrc or ~/.zshrc. Edit the variables below.
#
#   poly-tunnel-up    start SSH tunnel localhost:16379 -> VM:6379
#   poly-tunnel-down  kill tunnel
#   poly-tunnel       status + event count
#   poly-tui-remote   ensure tunnel, launch TUI against VM Redis
# =====================================================================

POLY_VM_HOST='ubuntu@<vm-ip>'                                                   # edit
POLY_TUNNEL_PORT=16379
POLY_TUI_BIN="$HOME/projects/poly/target/release/poly-tui"                       # edit
POLY_PID_FILE="${TMPDIR:-/tmp}/poly-tunnel.pid"

_poly_tunnel_pid() {
    [[ -f "$POLY_PID_FILE" ]] || return 1
    local pid; pid=$(cat "$POLY_PID_FILE" 2>/dev/null)
    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null && echo "$pid"
}

poly-tunnel-up() {
    if pid=$(_poly_tunnel_pid); then
        echo "Tunnel already up — PID $pid"
        return
    fi
    echo "Starting SSH tunnel localhost:${POLY_TUNNEL_PORT} -> ${POLY_VM_HOST}:6379 ..."
    ssh -N -f \
        -o BatchMode=yes \
        -o ServerAliveInterval=30 \
        -o ExitOnForwardFailure=yes \
        -L "${POLY_TUNNEL_PORT}:127.0.0.1:6379" \
        "$POLY_VM_HOST"
    # ssh -f forks to background; capture pid via pgrep
    sleep 1
    pid=$(pgrep -f "ssh -N.*${POLY_TUNNEL_PORT}:127.0.0.1:6379 ${POLY_VM_HOST}" | head -1)
    if [[ -n "$pid" ]]; then
        echo "$pid" > "$POLY_PID_FILE"
        echo "Tunnel up (PID $pid)."
    else
        echo "Tunnel failed to start."
    fi
}

poly-tunnel-down() {
    if pid=$(_poly_tunnel_pid); then
        kill "$pid"
        rm -f "$POLY_PID_FILE"
        echo "Tunnel stopped (was PID $pid)."
    else
        echo "No tunnel running."
    fi
}

poly-tunnel() {
    if pid=$(_poly_tunnel_pid); then
        echo "Tunnel: UP (PID $pid)"
        if command -v redis-cli >/dev/null 2>&1; then
            echo "Events on VM redis: $(redis-cli -p "$POLY_TUNNEL_PORT" XLEN poly:prod:trader:events)"
        fi
    else
        echo "Tunnel: DOWN"
    fi
}

poly-tui-remote() {
    _poly_tunnel_pid >/dev/null || poly-tunnel-up
    if [[ ! -x "$POLY_TUI_BIN" ]]; then
        echo "TUI binary not found: $POLY_TUI_BIN"
        echo "Build with: cargo build --release --bin poly-tui"
        return 1
    fi
    REDIS_URL="redis://127.0.0.1:${POLY_TUNNEL_PORT}" "$POLY_TUI_BIN"
}
