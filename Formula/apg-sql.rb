# T-SQL scanner frontend for apg. Publishes the ScriptDom-based sqlfrontend as
# a self-contained single-file binary and drops it into the shared frontends
# dir the `scanner` formula's bin/apg wrapper points at. Needs a .NET SDK at
# build time only; the published binary has no scan-time dependency.

class ApgSql < Formula
  desc "T-SQL scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.30.0",
      revision: "85b275f1707de9f9b8a93b741db20a2c0fd53e5b"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.30.0"
    rebuild 3
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "ff80ec654fb81bbbf5928e912c20d9cad47536ad6efd4a2d80717678efb8d069"
  end

  # A bottle block is merged here by the bottle workflow once this formula is
  # registered there and a tagged release publishes a bottle.

  depends_on "dotnet" => :build
  depends_on "scanner"

  def install
    rid = if Hardware::CPU.arm?
            "osx-arm64"
          else
            "osx-x64"
          end
    system "dotnet", "publish", "src/sqllib/SqlFrontend.csproj",
           "-c", "Release", "-r", rid, "--self-contained", "true",
           "-p:PublishSingleFile=true", "-p:PublishReadyToRun=true",
           "-o", "sql-dist"
    (share/"apg/frontends").install "sql-dist/sqlfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/sqlfrontend"
  end
end
