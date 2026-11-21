# T-SQL scanner frontend for apg. Publishes the ScriptDom-based sqlfrontend as
# a self-contained single-file binary and drops it into the shared frontends
# dir the `scanner` formula's bin/apg wrapper points at. Needs a .NET SDK at
# build time only; the published binary has no scan-time dependency.

class ApgSql < Formula
  desc "T-SQL scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.29.2",
      revision: "ee6363e0eef7024791fceeb21ded6dc7bd63fb28"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

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
