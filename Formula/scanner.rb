# Scanner formula — the `apg` binary (ingestor + query CLI). No scanner
# frontends are bundled; install the per-language frontend formulae
# (apg-go, apg-java, apg-cpp) which drop their artifacts into
# $(brew --prefix)/share/apg/frontends. The bin/apg wrapper points the binary
# at that directory via APG_FRONTEND_DIR.

class Scanner < Formula
  desc "Program graph scanner + LadybugDB query CLI for opencode"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.1.0",
      revision: "ccbe74867c20f60ec81e2ad0300aac28854a4c6d"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  depends_on "cmake" => :build   # lbug (vendored LadybugDB C++) builds via cmake
  depends_on "rust" => :build
  depends_on "openssl@3"         # lbug links libssl/libcrypto dynamically

  def install
    # Force lbug to build its vendored C++ source (the prebuilt download hits
    # the network, which brew's sandbox blocks).
    ENV["LBUG_BUILD_FROM_SOURCE"] = "1"
    # Do not compile any scanner frontends in this build; the separate
    # apg-go / apg-java / apg-cpp formulae provide them.
    ENV["APG_BUILD_FRONTENDS"] = "0"

    system "cargo", "install", *std_cargo_args

    # bin/apg is a small wrapper that points the binary at the shared
    # frontends dir populated by the per-language formulae.
    bin.write_env_script libexec/"bin"/"apg",
                         APG_FRONTEND_DIR: "#{HOMEBREW_PREFIX}/share/apg/frontends"
  end

  def caveats
    <<~EOS
      apg needs at least one scanner frontend. Install the ones you use:

        brew install antz29/apg/apg-go     # Go
        brew install antz29/apg/apg-java   # Java
        brew install antz29/apg/apg-cpp    # C++
    EOS
  end

  test do
    assert_match "apg #{version}", shell_output("#{bin}/apg --version")
    assert_match "USAGE", shell_output("#{bin}/apg --help")
  end
end
