#!/bin/sh
# Post-install hook: restart typio.service if running
# Meson runs this after installation with DESTDIR set (empty for system installs)

# Skip if cross-compiling or installing to a DESTDIR (packaging)
if [ -n "$DESTDIR" ]; then
    exit 0
fi

# Check if systemctl --user is available and service is active
if command -v systemctl >/dev/null 2>&1; then
    if systemctl --user is-active --quiet typio.service 2>/dev/null; then
        echo "Restarting typio.service..."
        systemctl --user restart typio.service
    elif systemctl --user is-enabled --quiet typio.service 2>/dev/null; then
        echo "typio.service is enabled but not running. Start it with: systemctl --user start typio.service"
    fi
fi
