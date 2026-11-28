# COBOL scanner frontend for apg. Builds the cobolfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain and crates.io (serde,
# serde_json) at build time. Scanning itself has NO scan-time dependency: the
# frontend never shells out.

class ApgCobol < Formula
  desc "COBOL scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.31.0",
      revision: "9c1c45aea62d5d26f4d13ec8931b2358906134f8"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.31.0"
    rebuild 9
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "fd157cbaf9ac030f917544b255f9290479d9feb3378fa787e3281a393acdfce0"
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
