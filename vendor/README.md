# Vendored dependencies

Copies of published crates with small fixes, wired in through
`[patch.crates-io]` in the workspace `Cargo.toml`. Each fix is marked
`tessela patch` in the source. Drop the copy once upstream has the fix.

| crate | version | fix |
|---|---|---|
| `gpui-linux-gpui-unofficial` | 1.23.2 | `src/linux/wayland/window.rs`: skip `wp_viewport.set_destination` while a size is 0. A layer surface stretched along an axis has size 0 there until configured; sway rejects it as a protocol error and kills the surface (Hyprland tolerates it). |
