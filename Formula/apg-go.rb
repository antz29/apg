# Go scanner frontend for apg. Builds the gofrontend binary and drops it into
# the shared frontends dir the `scanner` formula's bin/apg wrapper points at.

class ApgGo < Formula
  desc "Go scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.21.0",
      revision: "22e3df4f6607e9f09b3e9357c1a32bf3ac6d25ab"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.21.0"
    rebuild 65
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "3de18d9a2b8ceb0ca20bc9492ecb56393fd3ffac28ecca7ab1e9a38f521f3e0e"
  end

  depends_on "go" => :build
  depends_on "scanner"

  def install
    cd "src/golib" do
      system "go", "build", "-o", "gofrontend", "."
    end
    (share/"apg/frontends").install "src/golib/gofrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/gofrontend"
  end
end
