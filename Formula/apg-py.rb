# Python scanner frontend for apg. Builds the pyfrontend binary (Astral's `ty`
# type checker + Ruff parser crates, vendored from astral-sh/ruff at a pinned
# release tag — see src/pylib/Cargo.toml) and drops it into the shared
# frontends dir the `scanner` formula's bin/apg wrapper points at. Requires a
# current stable Rust toolchain and network at build time to fetch the pinned
# Ruff/ty crates. No Python runtime is required at scan time — pyfrontend is a
# self-contained native binary.

class ApgPy < Formula
  desc "Python scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.11.0",
      revision: "6e374e92b0cd6ef9675bfcc9b9b66d24e1ab9d17"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

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
