# RPM (Fedora)

`tessela.spec` builds the `tessela` runner (package `tessela-cli`, with the
GPUI renderer) into `/usr/bin/tessela`. It builds offline: the crates come
from a vendored tarball, not from crates.io.

## Building

```sh
# 1. Sources: a git archive of HEAD and every crate in Cargo.lock.
nix develop -c packaging/rpm/make-sources.sh ~/rpmbuild/SOURCES
#    (any revision: REF=<tag> packaging/rpm/make-sources.sh ...)

# 2. On Fedora (or in a container, see below):
sudo dnf builddep packaging/rpm/tessela.spec
rpmbuild -ba packaging/rpm/tessela.spec
```

In a container, from a machine without Fedora:

```sh
docker run --rm --cpus=3 -v ~/rpmbuild:/root/rpmbuild -v "$PWD/packaging/rpm:/specs:ro" \
    fedora:latest sh -c 'dnf -y install rpm-build && dnf -y builddep /specs/tessela.spec &&
                         rpmbuild -ba /specs/tessela.spec'
```

`rpmbuild --without check` skips the test suite.

## What the spec does

- `%prep` unpacks both tarballs; the crates land in `cargo-vendor/`
  (`vendor/` already holds the patched GPUI crate that `[patch.crates-io]`
  points at) and `%cargo_prep -v cargo-vendor` makes cargo use them.
- `%build` builds only `-p tessela-cli` with Fedora's `rpm` cargo profile and
  writes `LICENSE.dependencies` (`%cargo_license`) and `cargo-vendor.txt`
  (`%cargo_vendor_manifest`, which turns into `Provides: bundled(crate(...))`).
- `%check` runs `cargo test --workspace`. Every test is headless (the GPUI
  crate's tests build views without opening a window), so no compositor or
  GPU is needed in the build root. The SDK's (`sdk/rust`) tests run too: they
  need Unix sockets and `sh`, and the D-Bus one skips itself without a
  session bus.
- `apps/marathon` (the older example shell) is not packaged.

## Dependencies

Build: `rust`, `cargo`, `cargo-rpm-macros`, `gcc`, `gcc-c++` (mlua builds
Luau, rquickjs builds QuickJS), and the `-devel` packages behind
`pkgconfig(fontconfig)`, `pkgconfig(freetype2)`, `pkgconfig(xcb)`,
`pkgconfig(xkbcommon)`, `pkgconfig(xkbcommon-x11)`.

Runtime: fontconfig, freetype, libxcb and libxkbcommon are linked, so rpm
finds them itself. These are `dlopen`'d and therefore listed by hand:
`libwayland-client`, `libwayland-egl`, `vulkan-loader` and `libglvnd-egl`
(wgpu's GL fallback). A Vulkan driver (`mesa-vulkan-drivers`) is only
recommended, since NVIDIA's comes from its own driver; `sway` (for
`--nested`) and `grim` (for `devtools shot`) are suggested.

## License tag

tessela is `MIT OR Apache-2.0`; the binary links the vendored crates, so the
`License:` tag is their summary, from `%cargo_license_summary` (printed in
the build log). Update it when Cargo.lock changes.

The `URL:` is the provisional placeholder from Cargo.toml.
