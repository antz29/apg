# C# scanner frontend for apg. Publishes the Roslyn-based scanner as a
# self-contained single-file binary and drops it into the shared frontends dir
# the `scanner` formula's bin/apg wrapper points at. Needs a .NET SDK at build
# time; the published binary is self-contained (no runtime dependency).

class ApgCsharp < Formula
  desc "C# scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.13.1",
      revision: "24e2c49d7f998ed78475c15e419e0506d1403fcf"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.13.1"
    rebuild 22
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "82b8908fca37875234e89e21e4fa30148308c6ffb9c70d0566fbfa3e2943bf8e"
  end

  depends_on "dotnet" => :build
  depends_on "scanner"

  def install
    rid = if Hardware::CPU.arm?
            "osx-arm64"
          else
            "osx-x64"
          end
    system "dotnet", "publish", "src/csharplib/CsharpFrontend.csproj",
           "-c", "Release", "-r", rid, "--self-contained", "true",
           "-p:PublishSingleFile=true", "-p:PublishReadyToRun=true",
           "-o", "csharp-dist"
    (share/"apg/frontends").install "csharp-dist/csharpfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/csharpfrontend"
  end
end