# Rust scanner frontend for apg. Builds the rustfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain (rust-analyzer tracks the newest
# stable) and network at build time to fetch the pinned rust-analyzer crates.

class ApgRust < Formula
  desc "Rust scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.12.3",
      revision: "6c169e1359848b2d784c935fedb23a1bebf380fd"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.12.3"
    rebuild 23
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "9b589c18c9b534e188a26fd284ecb162451faa50f797b2bf9126afd4347cafaa"
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