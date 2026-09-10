class Sshr < Formula
  desc "Resilient SSH sessions with automatic reconnection"
  homepage "https://github.com/DoeringChristian/sshr"
  head "https://github.com/DoeringChristian/sshr.git", branch: "main"
  license "MIT"
  # no tagged release -> head-only; install with:  brew install --HEAD sshr
  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args

    # sshr finds the prebuilt shpool binaries by walking up from its own
    # executable looking for `shpool/bin` or `share/sshr/shpool/bin`, so they
    # have to land next to the installed binary rather than staying in the
    # source tree. Without this, uploading shpool to a fresh remote fails with
    # "no local shpool binaries found".
    (share/"sshr/shpool").install "shpool/bin"
    (share/"sshr/kitty").install Dir["kitty/*"]
  end

  def caveats
    <<~EOS
      The optional kitty kittens are installed in:
        #{opt_share}/sshr/kitty
    EOS
  end

  test do
    assert_match "sshr", shell_output("#{bin}/sshr --version")
  end
end
