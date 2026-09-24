# Rust scanner frontend for apg. Builds the rustfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain (rust-analyzer tracks the newest
# stable) and network at build time to fetch the pinned rust-analyzer crates.

class ApgRust < Formula
  desc "Rust scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.17.0",
      revision: "3c55e42b5e233fbdfec5026e59be50bf0c348624"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.17.0"
    rebuild 49
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "3f060d2f955d433d6204d7c78d2028f484972c28c673992bb17e8080c49acb6f"
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