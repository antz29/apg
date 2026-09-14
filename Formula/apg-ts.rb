# TypeScript scanner frontend for apg. Runs `npm ci` in src/tslib (the official
# TypeScript compiler) and drops the scanner + its node_modules into the shared
# frontends dir the `scanner` formula's bin/apg wrapper points at. Needs `node`
# on PATH at scan time; a repo's node_modules is always skipped, and
# workspace-package imports resolve even before `npm install`.

class ApgTs < Formula
  desc "TypeScript scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.13.2",
      revision: "bc3d16d2c4df7764099e8b71a8029bd7266266c9"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.13.2"
    rebuild 24
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "1fe345e85e9c7acb51977ed6555dc5afe678e68376ff7142b90034a44a281469"
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