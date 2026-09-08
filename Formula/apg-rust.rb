# Rust scanner frontend for apg. Builds the rustfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain (rust-analyzer tracks the newest
# stable) and network at build time to fetch the pinned rust-analyzer crates.

class ApgRust < Formula
  desc "Rust scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.10.4",
      revision: "26040fd683655940e4d6e75bf0bd850d505fac00"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.10.4"
    rebuild 14
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "04fb8823ead7c06f9f29d0ad10aec51e4202b80661b7fd28931ad7ea331df957"
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