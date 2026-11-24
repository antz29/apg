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
      tag:      "v0.30.0",
      revision: "85b275f1707de9f9b8a93b741db20a2c0fd53e5b"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.30.0"
    rebuild 12
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "29d2128a63cd08cc6808581c1f98f10c31ba29e9e58ca46d10b64910026f0cc7"
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
