# Markdown scanner frontend for apg. Builds the mdfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain at build time. Scanning itself
# needs no Markdown runtime and never shells out.

class ApgMd < Formula
  desc "Markdown scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.15.4",
      revision: "8f8561d6cddeccf811ba80972ffc19a106745230"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.15.4"
    rebuild 12
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "a0240f53fad6a311ae944d5be7c1061d8ee1b1c909bf9847526dadf27282fd13"
  end

  # A bottle block is merged here by the bottle workflow once this formula is
  # registered there and a tagged release publishes a bottle.

  depends_on "rust" => :build
  depends_on "scanner"

  def install
    cd "src/mdlib" do
      system "cargo", "build", "--release", "--bin", "mdfrontend"
    end
    (share/"apg/frontends").install "src/mdlib/target/release/mdfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/mdfrontend"
  end
end
