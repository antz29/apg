# Python scanner frontend for apg. Builds the pyfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain and network at build time to
# fetch the pinned Ruff/ty engine crates. Scanning itself needs NO Python
# runtime: the frontend resolves from filesystem markers (pyproject.toml /
# uv.lock / pyvenv.cfg) and never shells out to `python`/`uv`.

class ApgPy < Formula
  desc "Python scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.29.0",
      revision: "713781244afc05df34b4405eb9599f42c0f1dd53"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.29.0"
    rebuild 7
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "152c5430014a9eb7171b533b2383733ef24e0598acb97e2a9257567787fb0a4b"
  end

  # A bottle block is merged here by the bottle workflow once this formula is
  # registered there and a tagged release publishes a bottle.

  depends_on "rust" => :build
  depends_on "scanner"

  def install
    cd "src/pylib" do
      system "cargo", "build", "--release", "--bin", "pyfrontend"
    end
    (share/"apg/frontends").install "src/pylib/target/release/pyfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/pyfrontend"
  end
end
