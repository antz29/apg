# Rust scanner frontend for apg. Builds the rustfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain (rust-analyzer tracks the newest
# stable) and network at build time to fetch the pinned rust-analyzer crates.

class ApgRust < Formula
  desc "Rust scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.23.0",
      revision: "0772de837c504561db695d5797fe4a2ac4fbd72f"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.23.0"
    rebuild 71
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "82e1550118ad97ce8ef10546ccd103d94b54756ec4d4f18f96000dfc9a0e0559"
  end

  depends_on "rust" => :build
  depends_on "scanner"

  def install
    cd "src/rustlib" do
      system "cargo", "build", "--release", "--bin", "rustfrontend"
    end
    (share/"apg/frontends").install "src/rustlib/target/release/rustfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/rustfrontend"
  end
end