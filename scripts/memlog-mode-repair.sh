#!/usr/bin/env bash
# memlog-mode-repair.sh — install/uninstall/verify the udev mode fix for /dev/memlog
#
# Usage:
#   memlog-mode-repair.sh install    — write /etc/udev/rules.d/72-memlog-mode-fix.rules
#                                      and reload udev so /dev/memlog becomes 0660
#   memlog-mode-repair.sh uninstall  — remove the drop-in and reload udev
#   memlog-mode-repair.sh verify     — run memlog-mode-verify.sh
#
# Background: /usr/lib/udev/rules.d/70-linux-wintermute-memlog.rules ships MODE="0640"
# (wrong; a one-digit skew from the driver's 0660). This script installs a higher-priority
# /etc drop-in (72- sorts after 70-) that overrides the packaged rule. No reboot needed.
# The packaged source fix (pkg/linux-wintermute-memlog.rules → 0660) takes effect on the
# next linux-wintermute pkgrel rebuild; until then the /etc drop-in is the active bridge.

set -euo pipefail

DROPIN=/etc/udev/rules.d/72-memlog-mode-fix.rules
DROPIN_CONTENT='KERNEL=="memlog", GROUP="memlog", MODE="0660"'
DEVICE=/dev/memlog
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERIFY_SCRIPT="$SCRIPT_DIR/memlog-mode-verify.sh"

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

cmd_install() {
    # Check udevadm is available
    if ! command -v udevadm &>/dev/null; then
        fail "udevadm not found — is udev installed?"
    fi

    # Write drop-in (idempotent)
    local existing_content=""
    if [[ -f "$DROPIN" ]]; then
        existing_content=$(cat "$DROPIN")
    fi

    if [[ "$existing_content" == "$DROPIN_CONTENT" ]]; then
        echo "INFO: $DROPIN already has correct content (idempotent)"
    else
        echo "$DROPIN_CONTENT" | sudo tee "$DROPIN" > /dev/null
        echo "OK: wrote $DROPIN"
    fi

    # Reload and trigger
    sudo udevadm control --reload-rules
    sudo udevadm trigger --name-match=memlog
    sleep 0.3  # give udev a moment to apply

    # Verify live device
    if [[ ! -e "$DEVICE" ]]; then
        fail "$DEVICE does not exist after udevadm trigger"
    fi

    read -r mode group < <(stat -c '%a %G' "$DEVICE")
    if [[ "$mode" != "660" || "$group" != "memlog" ]]; then
        fail "$DEVICE is ${mode} ${group} after install — expected 660 memlog"
    fi

    echo "OK: $DEVICE is now mode=${mode} group=${group}"
    echo "DONE: install complete"
}

cmd_uninstall() {
    if [[ ! -f "$DROPIN" ]]; then
        echo "INFO: $DROPIN not present, nothing to uninstall"
        exit 0
    fi

    sudo rm -f "$DROPIN"
    echo "OK: removed $DROPIN"

    if command -v udevadm &>/dev/null; then
        sudo udevadm control --reload-rules
        sudo udevadm trigger --name-match=memlog 2>/dev/null || true
        echo "OK: udev rules reloaded"
    fi

    echo "NOTE: packaged source fix (pkg/linux-wintermute-memlog.rules → 0660) retained;"
    echo "      /dev/memlog will revert to 0640 until the linux-wintermute package is rebuilt."
    echo "DONE: uninstall complete"
}

cmd_verify() {
    if [[ ! -x "$VERIFY_SCRIPT" ]]; then
        fail "verify script not found or not executable: $VERIFY_SCRIPT"
    fi
    exec "$VERIFY_SCRIPT"
}

case "${1:-}" in
    install)   cmd_install ;;
    uninstall) cmd_uninstall ;;
    verify)    cmd_verify ;;
    *)
        echo "Usage: $(basename "$0") <install|uninstall|verify>" >&2
        exit 1
        ;;
esac
