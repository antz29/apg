# Apg formula — program graph scanner + LadybugDB query CLI for opencode.
#
# Tap usage (from this repo):
#   brew tap antz29/apg https://github.com/antz29/apg.git
#   brew install antz29/apg/apg
#
# NOTE: the bare `brew tap antz29/apg` form requires the repo to be named
# `homebrew-apg`; since this repo is `apg`, pass the URL explicitly.
# Fill in `sha256` for the pinned v0.1.0 tarball once the tag exists.
# Until then, install from HEAD:
#   brew install --HEAD antz29/apg/apg

class Apg < Formula
  desc "Program graph scanner + LadybugDB query CLI for opencode"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg/archive/refs/tags/v0.1.0.tar.gz"
  # sha256 "<FILL_ME — run `curl -sL <url> | shasum -a 256` after tagging v0.1.0>"
  license "MIT"
  head "https://github.com/antz29/apg.git"

  depends_on "cmake" => :build   # lbug (vendored LadybugDB C++) builds via cmake
  depends_on "gcc" => :build     # C++ scanner frontend (tree-sitter)
  depends_on "go" => :build      # Go scanner frontend
  depends_on "rust" => :build
  depends_on "openjdk"           # javac (build) + java (runtime for the Java frontend)
  depends_on "openssl@3"         # lbug links libssl/libcrypto dynamically

  def install
    # Force lbug to build its vendored C++ source (the prebuilt download hits
    # the network, which brew's sandbox blocks).
    ENV["LBUG_BUILD_FROM_SOURCE"] = "1"

    system "cargo", "install", *std_cargo_args
    bin.install Dir[libexec/"bin"/"apg"]

    # build.rs stages the frontends into target/release/frontends; install them
    # under libexec so `apg` finds them relative to itself at runtime
    # (<exe_dir>/../libexec/frontends).
    (libexec/"frontends").install Dir["target/release/frontends/*"]
  end

  def caveats
    <<~EOS
      The Go and C++ scanners are self-contained. Scanning Java projects needs
      `java` on your PATH; since openjdk is keg-only, either link it or export:

        export PATH="#{formula_opt_bin("openjdk")}:$PATH"

      In a project run `apg init` to create .apg/ and install the opencode
      apg_query plugin, then `apg scan` to build the graph.
    EOS
  end

  test do
    assert_match "apg #{version}", shell_output("#{bin}/apg --version")
    assert_match "USAGE", shell_output("#{bin}/apg --help")
  end
end
