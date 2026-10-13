{
  description = "tessela — dev shell (Rust, Go, Python 3, GPUI runtime deps)";

  inputs.nixpkgs.url = "flake:nixpkgs";

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          name = "tessela";

          # Toolchains: Rust for the framework, Go and Python for the example services.
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            go
            python3
            pkg-config
            # mlua (luau, vendored) and rquickjs compile C/C++ sources.
            stdenv.cc
          ];

          # Link-time deps. The GPUI renderer needs the Wayland/Vulkan stack.
          buildInputs = with pkgs; [
            wayland
            wayland-protocols
            libxkbcommon
            vulkan-loader
            libGL
            fontconfig
            freetype
            openssl
            zlib
            # GPUI also builds its X11 backend.
            libxcb
            libx11
          ];

          # Wayland, libxkbcommon and the Vulkan loader are dlopen'd at runtime,
          # so they have to be on the library path, not just the link line.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (
            with pkgs; [
              wayland
              libxcb
              libx11
              libxkbcommon
              vulkan-loader
              libGL
              fontconfig
              freetype
            ]
          );

          shellHook = ''
            echo "tessela dev shell — rust $(rustc --version | cut -d' ' -f2), go $(go version | cut -d' ' -f3), python $(python3 --version | cut -d' ' -f2)"
          '';
        };
      });
    };
}
