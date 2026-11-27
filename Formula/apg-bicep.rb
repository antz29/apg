# Bicep scanner frontend for apg. Builds the bicepfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain and crates.io (serde, serde_json)
# at build time, with no C compiler. Scanning itself has NO scan-time
# dependency: the frontend is self-contained and never shells out.

class ApgBicep < Formula
  desc "Bicep scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.30.0",
      revision: "85b275f1707de9f9b8a93b741db20a2c0fd53e5b"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.30.0"
  end

  # A bottle sha256 is merged here by the bottle workflow once this formula is
  # registered there and a tagged release publishes a bottle.

  depends_on "rust" => :build
  depends_on "scanner"

  def install
    cd "src/biceplib" do
      system "cargo", "build", "--release", "--bin", "bicepfrontend"
    end
    (share/"apg/frontends").install "src/biceplib/target/release/bicepfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/bicepfrontend"
  end
end
