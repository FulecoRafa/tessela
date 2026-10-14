# Flatpak

- `tessela.yml`: the runner as a flatpak-builder **module**. Apps that bundle
  tessela include it (adwaita-shell's manifest does, from a sibling
  checkout). It builds `tessela-cli` with the `rust-stable` SDK extension,
  offline, from `cargo-sources.json`.
- `io.github.fuleco.Tessela.yml`: the runner on its own, as an app:
  `flatpak run io.github.fuleco.Tessela run ~/my-project`. Its command is
  `tessela-flatpak.sh`, which restores `WAYLAND_DISPLAY` (see below).
- `cargo-sources.json`: every crate in `Cargo.lock`, generated. Regenerate it
  whenever `Cargo.lock` changes:

  ```sh
  curl -LO https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
  nix shell --impure --expr 'with import (builtins.getFlake "nixpkgs") {};
      python3.withPackages (p: [ p.aiohttp p.tomlkit ])' \
    -c python3 flatpak-cargo-generator.py Cargo.lock -o packaging/flatpak/cargo-sources.json
  ```

## Building

Runtime `org.freedesktop.Platform//26.08` and its SDK, plus
`org.freedesktop.Sdk.Extension.rust-stable//26.08`, from Flathub:

```sh
flatpak-builder --user --install-deps-from=flathub --install --force-clean \
    --state-dir ~/.cache/flatpak-builder build-dir packaging/flatpak/io.github.fuleco.Tessela.yml
flatpak run io.github.fuleco.Tessela --help
```

Build outside the repository (the module's `dir` source skips `target/`,
`.git` and `.flatpak-builder`, but a build dir inside the tree would be
copied into the next build).

## Should you use it?

Mostly not. tessela draws desktop surfaces for the session, and a project's
plugins call the host's tools; a sandbox gets in the way of both. Prefer the
Nix package or the RPM. What this manifest grants and what still breaks:

| | |
|---|---|
| Wayland | **not** `--socket=wayland`: Flatpak hands that socket out through `wp_security_context_v1`, and Hyprland and sway then hide `zwlr_layer_shell_v1` from the app, so no surface opens. The app gets the compositor's own socket (`--filesystem=xdg-run/wayland-0`, `wayland-1`) and its command, `tessela-flatpak.sh`, sets `WAYLAND_DISPLAY` again (Flatpak unsets it). Another name: `flatpak run --filesystem=xdg-run/wayland-2 --env=TESSELA_WAYLAND_DISPLAY=wayland-2 io.github.fuleco.Tessela run ...`. |
| GPU | `--device=dri`; Mesa from the runtime's GL extension. |
| projects | `--filesystem=home:ro`: a project anywhere in the home directory. |
| Hyprland module | `--filesystem=xdg-run/hypr` (its IPC sockets). |
| tray | `--socket=session-bus` (it talks to the watcher and to every item). |
| plugins | run **inside** the sandbox with the runtime's `python3`; a plugin that runs host tools (`wpctl`, `nmcli`, ...) finds none. Bundle the shell as its own app instead (see adwaita-shell's `packaging/flatpak`, which forwards them to the host). |
| apps module | sees only desktop entries visible in the sandbox; launching runs inside it. |
| `--nested` | needs `sway` on PATH: not available in the sandbox. |
| devtools | the runner's sockets live in the sandbox's private `$XDG_RUNTIME_DIR`, so `tessela devtools` on the host cannot reach them; run it inside the running instance: `flatpak enter <instance> tessela devtools ...` (`flatpak ps` lists instances). |
| state | `$XDG_STATE_HOME` is `~/.var/app/io.github.fuleco.Tessela/.local/state`, not `~/.local/state`. |
| fonts | the runtime's, `~/.local/share/fonts` and the host's `/usr/share/fonts`. A project's icon font has to be installed there (or bundled by an app built on `tessela.yml`). |
| time zone on NixOS | `/etc/localtime` points into the Nix store and Flatpak falls back to UTC: `flatpak override --user --env=TZ=<zone> io.github.fuleco.Tessela`. |

## Verified

Built with flatpak-builder 1.4.4 against 26.08 on NixOS. `flatpak run
io.github.fuleco.Tessela --help` works, and `tessela run` of a copy of
adwaita-shell in a headless sway opened its bar (`opened ...-bar`, checked
with a screenshot); its plugins could not reach the host tools, as expected.
