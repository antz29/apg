# TypeScript scanner frontend for apg. Runs `npm ci` in src/tslib (the official
# TypeScript compiler) and drops the scanner + its node_modules into the shared
# frontends dir the `scanner` formula's bin/apg wrapper points at. Needs `node`
# on PATH at scan time; a repo's node_modules is always skipped, and
# workspace-package imports resolve even before `npm install`.

class ApgTs < Formula
  desc "TypeScript scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.11.2",
      revision: "958ee7a41f2ac53bda25680f66c640993cff880e"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.11.2"
    rebuild 12
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "61a10d8fde476ef2bc31d021ed0d8b1b72b297356b72a2576d1fdb1d6176a6f1"
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