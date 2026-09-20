# TypeScript scanner frontend for apg. Runs `npm ci` in src/tslib (the official
# TypeScript compiler), transpiles the committed `scanner.ts` to `scanner.mjs`,
# and drops the built scanner + its node_modules into the shared frontends dir
# the `scanner` formula's bin/apg wrapper points at. Needs `node` on PATH at
# scan time; a repo's node_modules is always skipped, and workspace-package
# imports resolve even before `npm install`.

class ApgTs < Formula
  desc "TypeScript scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.15.1",
      revision: "e19f5e5531f5cf599314ee2d2b3f77830889d8f2"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.15.1"
    rebuild 34
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "72dba77d4774f28ffd618719205e475c28c401aaaf0b21f20c0c7358aae7727e"
  end

  depends_on "node" # npm ci to fetch typescript at build; node at scan time
  depends_on "scanner"

  def install
    cd "src/tslib" do
      system "npm", "ci", "--no-audit", "--no-fund"
    end

    # `build.rs` transpiles the committed `scanner.ts` (transpile-only,
    # `@ts-nocheck`) to `scanner.mjs` during a cargo build. The formula does not
    # run that build script, so mirror the transpile here — otherwise the staged
    # frontend is source-only (`scanner.ts`) and `apg` dies with MODULE_NOT_FOUND
    # running `node <dir>/tsfrontend/scanner.mjs` (see `frontend_cmd` in
    # src/main.rs). Flags must match build.rs.
    system "node", "src/tslib/node_modules/typescript/bin/tsc",
           "src/tslib/scanner.ts",
           "--module", "esnext",
           "--target", "esnext",
           "--moduleResolution", "bundler",
           "--allowJs", "false",
           "--skipLibCheck",
           "--noEmitOnError",
           "--outDir", buildpath/"tsfrontend"

    (share/"apg/frontends").install "src/tslib" => "tsfrontend"
    cp buildpath/"tsfrontend/scanner.js", share/"apg/frontends/tsfrontend/scanner.mjs"
  end

  test do
    assert_path_exists share/"apg/frontends/tsfrontend/scanner.mjs"
    system "node", "--check", share/"apg/frontends/tsfrontend/scanner.mjs"
  end
end