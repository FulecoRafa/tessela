# tessela: the runner (`tessela run <project>`) and its devtools, with the
# GPUI renderer. Written callPackage-style so it can move to nixpkgs; there,
# replace `src` with a fetchFromGitHub of a tagged release.
{
  lib,
  rustPlatform,
  pkg-config,
  fontconfig,
  freetype,
  libGL,
  libx11,
  libxcb,
  libxkbcommon,
  vulkan-loader,
  wayland,
}:

let
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;

  # Libraries GPUI dlopen()s at runtime instead of linking: they have to be
  # on the binary's RUNPATH, or it only starts inside `nix develop`.
  runtimeLibs = [
    wayland
    libxkbcommon
    vulkan-loader
    libGL
    libxcb
    libx11
    fontconfig
    freetype
  ];
in
rustPlatform.buildRustPackage {
  pname = "tessela";
  inherit (workspace) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../apps
      ../crates
      # tessela-service (the Rust SDK): a workspace member, so cargo needs it
      # even to build the runner alone; projects build their services on it.
      ../sdk/rust
      # Local fixes to crates.io dependencies, wired in by [patch.crates-io].
      ../vendor
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  # Only the runner; apps/marathon is an example, not something to install.
  cargoBuildFlags = [ "--package" "tessela-cli" ];

  nativeBuildInputs = [
    pkg-config
  ];

  buildInputs = runtimeLibs;

  # The workspace's default members (core, engines, sandbox, modules, TUI,
  # the SDK) and the runner. They need no display: GPUI is only exercised by
  # the runner's binary, never by a test. The SDK's D-Bus test skips itself
  # without a session bus, as in the sandbox.
  cargoTestFlags = [ "--workspace" "--exclude" "marathon" ];

  # Prefer RUNPATH over a wrapper setting LD_LIBRARY_PATH: the runner starts
  # plugins and launches apps, which must not inherit our library path.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/tessela
  '';

  meta = {
    description = "Dynamic UI and extension framework: desktop shells from sandboxed TSX/Luau projects";
    homepage = workspace.repository;
    license = with lib.licenses; [ mit asl20 ];
    mainProgram = "tessela";
    platforms = lib.platforms.linux;
  };
}
