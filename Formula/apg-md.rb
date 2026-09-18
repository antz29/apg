# Markdown scanner frontend for apg. Builds the mdfrontend binary and drops it
# into the shared frontends dir the `scanner` formula's bin/apg wrapper points
# at. Requires a current stable Rust toolchain at build time. Scanning itself
# needs no Markdown runtime and never shells out.

class ApgMd < Formula
  desc "Markdown scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.14.0",
      revision: "a02a58d0db9c6f81879bccb3729d5376f7b88d59"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

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
