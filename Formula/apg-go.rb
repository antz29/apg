# Go scanner frontend for apg. Builds the gofrontend binary and drops it into
# the shared frontends dir the `scanner` formula's bin/apg wrapper points at.

class ApgGo < Formula
  desc "Go scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.13.3",
      revision: "19298817359f645414146ae3847ba5217087db0a"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.13.3"
    rebuild 31
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "dd102b47b51e83202e769ca520f12b2b629aadb805cd65adcb4bdb6321b20140"
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
