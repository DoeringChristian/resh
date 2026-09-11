class Resh < Formula
  desc "Resilient SSH sessions with automatic reconnection"
  homepage "https://github.com/DoeringChristian/resh"
  head "https://github.com/DoeringChristian/resh.git", branch: "main"
  license "MIT"
  # no tagged release -> head-only; install with:  brew install --HEAD resh
  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args

    # resh finds the prebuilt shpool binaries by walking up from its own
    # executable looking for `shpool/bin` or `share/resh/shpool/bin`, so they
    # have to land next to the installed binary rather than staying in the
    # source tree. Without this, uploading shpool to a fresh remote fails with
    # "no local shpool binaries found".
    (share/"resh/shpool").install "shpool/bin"
    (share/"resh/kitty").install Dir["kitty/*"]
  end

  def caveats
    <<~EOS
      The optional kitty kittens are installed in:
        #{opt_share}/resh/kitty
    EOS
  end

  test do
    assert_match "resh", shell_output("#{bin}/resh --version")
  end
end
