# COBOL scanner frontend for apg. Builds the cobolfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain and crates.io (serde,
# serde_json) at build time. Scanning itself has NO scan-time dependency: the
# frontend never shells out.

class ApgCobol < Formula
  desc "COBOL scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.30.0",
      revision: "85b275f1707de9f9b8a93b741db20a2c0fd53e5b"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.30.0"
    rebuild 7
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "dd87405bcf0086ef96f610a8dcb9b862dd309f21524f629625248b9f019b9fb2"
  end

  # A bottle block is merged here by the bottle workflow once this formula is
  # registered there and a tagged release publishes a bottle.

  depends_on "rust" => :build
  depends_on "scanner"

  def install
    cd "src/cobollib" do
      system "cargo", "build", "--release", "--bin", "cobolfrontend"
    end
    (share/"apg/frontends").install "src/cobollib/target/release/cobolfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/cobolfrontend"
  end
end
