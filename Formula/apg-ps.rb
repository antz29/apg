# PowerShell scanner frontend for apg. Builds the psfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain, crates.io (tree-sitter =0.26.13,
# woodruffw-trough-tree-sitter-powershell =0.1.6, the one crate exporting
# tree_sitter_powershell) and a C compiler at build time: it compiles the
# tree-sitter runtime and the woodruffw-trough grammar's generated C sources.
# Scanning itself has NO scan-time dependency: the parser is statically linked
# and the frontend never shells out.

class ApgPs < Formula
  desc "PowerShell scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.31.0",
      revision: "9c1c45aea62d5d26f4d13ec8931b2358906134f8"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.31.0"
    rebuild 4
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "969161179bc69ae456b45c07a084893ef95ab2d9d0338ed25119e5ed5460465b"
  end

  # A bottle block is merged here by the bottle workflow once this formula is
  # registered there and a tagged release publishes a bottle.

  depends_on "rust" => :build
  depends_on "scanner"

  def install
    cd "src/pslib" do
      system "cargo", "build", "--release", "--bin", "psfrontend"
    end
    (share/"apg/frontends").install "src/pslib/target/release/psfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/psfrontend"
  end
end
