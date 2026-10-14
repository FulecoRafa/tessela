# Homebrew formula for tessela (Linux only), for a tap, e.g.
#   brew tap FulecoRafa/tessela <git url of a tap repo holding this file>
#   brew install tessela
# See README.md next to this file for how to release and test it.
class Tessela < Formula
  desc "Runs tessela projects: desktop shells from sandboxed TSX/Luau extensions"
  # Placeholders until the project is published: point `url` at the release
  # tarball (`git archive --prefix=tessela-0.1.0/ -o tessela-0.1.0.tar.gz v0.1.0`)
  # and set `sha256` to `shasum -a 256 tessela-0.1.0.tar.gz`.
  homepage "https://github.com/FulecoRafa/tessela"
  url "https://github.com/FulecoRafa/tessela/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license any_of: ["MIT", "Apache-2.0"]

  depends_on "pkgconf" => :build
  depends_on "rust" => :build
  depends_on "fontconfig"
  depends_on "freetype"
  # From here on, what GPUI dlopen()s at runtime (Homebrew's RPATH finds them).
  depends_on "libx11"
  depends_on "libxcb"
  depends_on "libxkbcommon"
  # Wayland layer-shell surfaces, prctl, Hyprland/D-Bus desktop modules: the
  # runner does not build on macOS.
  depends_on :linux
  depends_on "mesa"
  depends_on "vulkan-loader"
  depends_on "wayland"

  def install
    # Only the runner; apps/marathon is an example.
    system "cargo", "install", *std_cargo_args(path: "apps/tessela")
    doc.install "README.md", "docs/ARCHITECTURE.md"
  end

  test do
    assert_match "tessela run <project>", shell_output("#{bin}/tessela --help")

    # A one-surface project, run without a display.
    (testpath/"hello/tessela.toml").write <<~TOML
      name = "brew-test"
      [surfaces.main]
      entry = "main.tsx"
    TOML
    (testpath/"hello/main.tsx").write <<~TSX
      export default <Text text="hello from brew" />;
    TSX
    ENV["XDG_RUNTIME_DIR"] = testpath/"run"
    ENV["XDG_STATE_HOME"] = testpath/"state"
    output = shell_output("#{bin}/tessela run #{testpath}/hello --headless --seconds 1 2>&1")
    assert_match "hello from brew", output
  end
end
