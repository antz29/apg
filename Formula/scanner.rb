# Scanner formula — the `apg` binary (ingestor + query CLI). No scanner
# frontends are bundled; install the per-language frontend formulae
# (apg-go, apg-java, apg-cpp) which drop their artifacts into
# $(brew --prefix)/share/apg/frontends. The bin/apg wrapper points the binary
# at that directory via APG_FRONTEND_DIR.

class Scanner < Formula
  desc "Program graph scanner + LadybugDB query CLI for opencode"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.3.0",
      revision: "cdba34d0baeddba753e0a2e0ddbde7bc00b552a8"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.3.0"
    # sha256 lines are filled in by the .github/workflows/bottle.yml build.
  end

  depends_on "rust" => :build
  depends_on "openssl@3"         # lbug links libssl/libcrypto dynamically

  # LadybugDB (the `lbug` Rust crate's C++ engine) is linked as a prebuilt
  # static library. Building it from source needs network + a huge C++ compile,
  # so the crate ships prebuilt libs on the LadybugDB GitHub release. Homebrew
  # fetches this resource before the sandbox (the crate's own in-build download
  # would be blocked), and lbug's build.rs links it via
  # LBUG_LIBRARY_DIR/LBUG_INCLUDE_DIR.
  resource "lbug" do
    if Hardware::CPU.arm?
      url "https://github.com/LadybugDB/ladybug/releases/download/v0.19.1/liblbug-static-osx-arm64.tar.gz"
      sha256 "9d8bf7fd2a2b715e419db1f087f57777fd9413e214abdf32fa60ca3a9e51d883"
    else
      url "https://github.com/LadybugDB/ladybug/releases/download/v0.19.1/liblbug-static-osx-x86_64.tar.gz"
      sha256 "8ae8597da0295b14a06ee89cb632ab44c5f0e834be9576689d706eea16159f79"
    end
  end

  def install
    # Point lbug's build.rs at the prebuilt static lib (the "lbug" resource)
    # instead of downloading or compiling from source. It takes the "external"
    # prebuilt path when both variables are set.
    lbug_dir = buildpath/"lbug-static"
    resource("lbug").stage(lbug_dir)
    ENV["LBUG_LIBRARY_DIR"] = lbug_dir.to_s
    ENV["LBUG_INCLUDE_DIR"] = lbug_dir.to_s

    # Do not compile any scanner frontends in this build; the separate
    # apg-go / apg-java / apg-cpp formulae provide them.
    ENV["APG_BUILD_FRONTENDS"] = "0"

    # Install the real binary into libexec (not bin/), then bin/apg becomes a
    # small wrapper that points the binary at the shared frontends dir
    # populated by the per-language formulae.
    system "cargo", "install", *std_cargo_args(root: libexec)

    (bin/"apg").write_env_script libexec/"bin"/"apg",
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
