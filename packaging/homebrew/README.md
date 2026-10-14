# Homebrew

`tessela.rb` is a formula for a tap. It is **Linux only** (Homebrew on Linux,
e.g. on Fedora atomic / Universal Blue images, where brew is the usual way to
add user-level tools without layering RPMs).

## Why not macOS

The runner does not build there and would be of no use if it did:

- surfaces are Wayland layer-shell windows (GPUI's Linux backend);
- `apps/tessela` calls `prctl(PR_SET_PDEATHSIG)` unconditionally;
- the always-on desktop modules read Hyprland's sockets, `/sys` and the
  freedesktop D-Bus services.

A `--no-default-features` build only drops the GPUI renderer, keeping the
headless one: enough for tests, not for an end user.

## Why there is no adwaita-shell formula

adwaita-shell is a session component, not a tool: it needs a systemd user unit
bound to `graphical-session.target` (`brew services` units are bound to
`default.target`, which starts before the compositor exports
`WAYLAND_DISPLAY`), and its plugins drive system services (NetworkManager,
BlueZ, PipeWire, power-profiles-daemon) whose CLIs must match the host's
daemons, so brew could not supply them anyway. Ship it as an RPM (layered or
baked into the image) or with Nix; with brew's tessela, `tessela run
<checkout>` also works.

## Releasing

1. Tag a release and make the tarball the formula points at:
   `git archive --prefix=tessela-0.1.0/ -o tessela-0.1.0.tar.gz v0.1.0`
2. Set `url` (where the tarball is published), `homepage` and
   `sha256` (`sha256sum tessela-0.1.0.tar.gz`) in `tessela.rb`.
3. Put `tessela.rb` in `Formula/` of a tap repository (`homebrew-tessela`);
   users then run `brew install FulecoRafa/tessela/tessela`.

## Testing locally (in Docker)

Recent brew only installs formulae from taps, so make a local one:

```sh
git archive --prefix=tessela-0.1.0/ -o /tmp/brewtest/tessela-0.1.0.tar.gz HEAD
sed -e "s|^  url .*|  url \"file:///src/tessela-0.1.0.tar.gz\"|" \
    -e "s|^  sha256 .*|  sha256 \"$(sha256sum /tmp/brewtest/tessela-0.1.0.tar.gz | cut -d' ' -f1)\"|" \
    packaging/homebrew/tessela.rb > /tmp/brewtest/tessela.rb
docker run --rm --cpus=3 -v /tmp/brewtest:/src homebrew/brew bash -c '
  brew tap-new --no-git local/test &&
  cp /src/tessela.rb "$(brew --repository local/test)/Formula/" &&
  HOMEBREW_MAKE_JOBS=3 brew install --build-from-source local/test/tessela &&
  brew test local/test/tessela'
```

Verified this way (Homebrew 2026-10, x86_64): `brew audit --strict` is
clean, the build takes ~19 min on 3 CPUs and `brew test` passes. `brew
linkage` lists `libx11` and `vulkan-loader` under "no linkage": expected,
GPUI dlopen()s them, and the RUNPATH brew sets covers them (and wayland, mesa).
That was before `sdk/rust` (tessela-service) joined the workspace; not
rebuilt since. The formula should not need a change: the release tarball
(`git archive`) contains `sdk/rust`, `cargo install --locked` of
`apps/tessela` resolves against the workspace's `Cargo.lock` (which has the
SDK's crates; `cargo metadata --locked` on the archive of the current
HEAD succeeds) and only builds what the runner depends on.
