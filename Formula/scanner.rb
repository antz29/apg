# Scanner formula — the `apg` binary (ingestor + query CLI) plus the bundled
# structural scanner. The structural scanner is built from src/structlib and
# installed into the shared frontends dir alongside the per-language frontend
# formulae (apg-go, apg-java, apg-cpp, apg-rust, apg-ts, apg-csharp, apg-py),
# which drop their artifacts into $(brew --prefix)/share/apg/frontends. The
# bin/apg wrapper points the binary at that directory via APG_FRONTEND_DIR.

class Scanner < Formula
  desc "Program graph scanner + LadybugDB query CLI for opencode"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.28.4",
      revision: "92b0179e65ff02f37482e7fd3e1ed0e560bef250"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.28.4"
    rebuild 6
    sha256 cellar: :any, arm64_sequoia: "d45ac7bdfff17be491be270debc4b8616cb33ad7f7e30bb650319c0de1659f4a"
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
      url "https://github.com/LadybugDB/ladybug/releases/download/v0.22.3/liblbug-static-osx-arm64.tar.gz"
      sha256 "9a6e6903ed36ad58fbc31412cf4aae6d269b90219137a8e29b47472a89c350c8"
    else
      url "https://github.com/LadybugDB/ladybug/releases/download/v0.22.3/liblbug-static-osx-x86_64.tar.gz"
      sha256 "8220ab819917245ff45bbd61bb5603fdf9d8a99d2697fdc092585e9ce031d359"
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

    # Do not compile any per-language scanner frontends in this build; the
    # separate apg-go / apg-java / apg-cpp / apg-rust / apg-ts / apg-csharp /
    # apg-py formulae provide them. The two bundled frontends (the structural
    # scanner from src/structlib and the agent frontend from src/agentlib) are
    # built separately below.
    ENV["APG_BUILD_FRONTENDS"] = "0"

    # Install the real binary into libexec (not bin/), then bin/apg becomes a
    # small wrapper that points the binary at the shared frontends dir
    # populated by the per-language formulae.
    system "cargo", "install", *std_cargo_args(root: libexec)

    (bin/"apg").write_env_script libexec/"bin"/"apg",
                                 APG_FRONTEND_DIR: "#{HOMEBREW_PREFIX}/share/apg/frontends"

    # Two bundled frontends ship in this base package, both built directly from
    # their own standalone, non-workspace crates and dropped into the shared
    # frontends dir beside the per-language frontends.
    #
    # 1. The bundled structural scanner: ONE `structfrontend` binary serving
    # the `md` stream plus every per-format structural stream and the residual
    # `misc`. Scanning with it needs no runtime and never shells out.
    cd "src/structlib" do
      system "cargo", "build", "--release", "--bin", "structfrontend"
    end
    (share/"apg/frontends").install "src/structlib/target/release/structfrontend"

    # 2. The bundled agent frontend: `agentfrontend` claims the
    # `.opencode/agents/` definitions. Like the structural scanner it needs no
    # runtime at scan time.
    cd "src/agentlib" do
      system "cargo", "build", "--release", "--bin", "agentfrontend"
    end
    (share/"apg/frontends").install "src/agentlib/target/release/agentfrontend"
  end

  def caveats
    <<~EOS
      Two bundled frontends ship with this formula — no separate install is
      needed:

        - the bundled structural scanner (Markdown plus shell, YAML, JSON,
          TOML, XML, Dockerfile, Makefile, INI and other text files)
        - the bundled agent frontend, which claims `.opencode/agents/`
          definitions

      apg also needs at least one code scanner frontend. Install the ones you use:

        brew install antz29/apg/apg-go       # Go
        brew install antz29/apg/apg-java     # Java
        brew install antz29/apg/apg-cpp      # C++
        brew install antz29/apg/apg-rust     # Rust
        brew install antz29/apg/apg-ts       # TypeScript (needs `node` at scan time)
        brew install antz29/apg/apg-csharp   # C# (needs `dotnet` at build time only)
        brew install antz29/apg/apg-py       # Python (no Python runtime needed at scan time)
    EOS
  end

  test do
    assert_match "apg #{version}", shell_output("#{bin}/apg --version")
    assert_match "USAGE", shell_output("#{bin}/apg --help")
    assert_path_exists share/"apg/frontends/structfrontend"
    assert_path_exists share/"apg/frontends/agentfrontend"
  end
end
