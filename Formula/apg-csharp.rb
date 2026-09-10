# C# scanner frontend for apg. Publishes the Roslyn-based scanner as a
# self-contained single-file binary and drops it into the shared frontends dir
# the `scanner` formula's bin/apg wrapper points at. Needs a .NET SDK at build
# time; the published binary is self-contained (no runtime dependency).

class ApgCsharp < Formula
  desc "C# scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.11.2",
      revision: "958ee7a41f2ac53bda25680f66c640993cff880e"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.11.2"
    rebuild 12
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "c9acd5512b12ac673099e71ebff08e00c38dca2ceab5ecdb26162083a5966627"
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