#!/bin/sh
# tessela inside the Flatpak sandbox (the app's command; arguments go to
# tessela).
#
# The app gets the compositor's own Wayland socket, not --socket=wayland:
# Flatpak hands that one out through security-context-v1, and Hyprland and
# sway then hide zwlr_layer_shell_v1 from the app. Without --socket=wayland
# Flatpak unsets WAYLAND_DISPLAY, so set it again: TESSELA_WAYLAND_DISPLAY,
# or the first of wayland-0, wayland-1 shared with the sandbox.
if [ -n "${TESSELA_WAYLAND_DISPLAY:-}" ]; then
    export WAYLAND_DISPLAY="$TESSELA_WAYLAND_DISPLAY"
else
    for d in wayland-0 wayland-1; do
        if [ -S "$XDG_RUNTIME_DIR/$d" ]; then
            export WAYLAND_DISPLAY="$d"
            break
        fi
    done
fi
exec tessela "$@"
