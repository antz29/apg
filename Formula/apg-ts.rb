# TypeScript scanner frontend for apg. Runs `npm ci` in src/tslib (the official
# TypeScript compiler) and drops the scanner + its node_modules into the shared
# frontends dir the `scanner` formula's bin/apg wrapper points at. Needs `node`
# on PATH at scan time; a repo's node_modules is always skipped, and
# workspace-package imports resolve even before `npm install`.

class ApgTs < Formula
  desc "TypeScript scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.9.0",
      revision: "b34aae57e140106e437e3d6baa5a2d5b5ca26ace"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.9.0"
    rebuild 3
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "f843639e3e4ec012736cda2080a7e7510077a93edcb887f199dd4e43f874f230"
  end

  depends_on "node" # npm ci to fetch typescript at build; node at scan time
  depends_on "scanner"

  def install
    cd "src/tslib" do
      system "npm", "ci", "--no-audit", "--no-fund"
    end
    (share/"apg/frontends").install "src/tslib" => "tsfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/tsfrontend/scanner.mjs"
  end
end