class Resh < Formula
  desc "Resilient SSH sessions with automatic reconnection"
  homepage "https://github.com/DoeringChristian/resh"
  url "https://github.com/DoeringChristian/resh/archive/refs/tags/v0.2.0.tar.gz"
  sha256 "b52c39c87348d301d38a16c6be657d59239a35bc089e8a3c80a9c22180498d59"
  license "MIT"
  head "https://github.com/DoeringChristian/resh.git", branch: "main"
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
