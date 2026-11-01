# C# scanner frontend for apg. Publishes the Roslyn-based scanner as a
# self-contained single-file binary and drops it into the shared frontends dir
# the `scanner` formula's bin/apg wrapper points at. Needs a .NET SDK at build
# time; the published binary is self-contained (no runtime dependency).

class ApgCsharp < Formula
  desc "C# scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.28.3",
      revision: "f35ae1ad56d83c113b866e7434ba1179d8a97905"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.28.3"
    rebuild 3
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "3417538b259ef12b01b9a92db25df8a2c4e54e30a6bee941ab713977b48d3363"
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