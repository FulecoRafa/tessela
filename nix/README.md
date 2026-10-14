# Nix

`flake.nix` exports:

| output | what |
|---|---|
| `packages.<system>.tessela` (= `default`) | the runner, `bin/tessela`, built from `nix/package.nix` |
| `apps.<system>.default` | `nix run . -- run <project>` |
| `overlays.default` | adds `pkgs.tessela` |
| `devShells.<system>.default` | Rust, Go, Python, sway/grim and the GPUI libraries |

```sh
nix build                          # ./result/bin/tessela
nix run . -- run ../adwaita-shell  # run a project
nix flake check
```

## The package

`nix/package.nix` is written callPackage-style so it can go to nixpkgs as is,
with `src` replaced by a `fetchFromGitHub` of a release.

- Builds only `tessela-cli` (the `tessela` binary, with the GPUI renderer).
  `apps/marathon` is an example and is not installed.
- `vendor/gpui-linux-gpui-unofficial` (wired in by `[patch.crates-io]`) is part
  of the source, so `cargoLock.lockFile` needs no extra hashes.
- GPUI `dlopen()`s Wayland, libxkbcommon, the Vulkan loader, libGL and X11
  instead of linking them. They are added to the binary's RUNPATH (`patchelf
  --add-rpath`) rather than set in `LD_LIBRARY_PATH` by a wrapper, which would
  leak into the plugins and apps the runner starts.
- `sdk/rust` (tessela-service, the Rust SDK) is in the source too: it is a
  workspace member, and projects that build their services on it (e.g.
  adwaita-shell) take it from this package's `src`.
- The check phase runs the workspace's tests (all but `marathon`, SDK
  included); none needs a display. The SDK's D-Bus test skips itself without
  a session bus.
- Not on the runtime closure, on purpose: `sway` (only for `--nested`) and
  `grim` (only for `tessela devtools shot`). Use the dev shell for those.

## Using it from another flake

```nix
inputs.tessela.url = "github:FulecoRafa/tessela";
# then either
environment.systemPackages = [ inputs.tessela.packages.${pkgs.stdenv.hostPlatform.system}.default ];
# or
nixpkgs.overlays = [ inputs.tessela.overlays.default ];  # pkgs.tessela
```
