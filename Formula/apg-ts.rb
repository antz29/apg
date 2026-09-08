# TypeScript scanner frontend for apg. Runs `npm ci` in src/tslib (the official
# TypeScript compiler) and drops the scanner + its node_modules into the shared
# frontends dir the `scanner` formula's bin/apg wrapper points at. Needs `node`
# on PATH at scan time; a repo's node_modules is always skipped, and
# workspace-package imports resolve even before `npm install`.

class ApgTs < Formula
  desc "TypeScript scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.10.4",
      revision: "26040fd683655940e4d6e75bf0bd850d505fac00"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.10.4"
    rebuild 8
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "1a09b3a815d11364c2059ff4f6ad266d445b2a4c8cc03929d2a1b3066b456494"
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