# Rust scanner frontend for apg. Builds the rustfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain (rust-analyzer tracks the newest
# stable) and network at build time to fetch the pinned rust-analyzer crates.

class ApgRust < Formula
  desc "Rust scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.28.2",
      revision: "bed2e0afee5711d105533dc5a28c43d04996321a"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.28.2"
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "7a3e9dca7b67732afc223e840d8caaf5586b848f07b5b4481eb8358080067a71"
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