# Go scanner frontend for apg. Builds the gofrontend binary and drops it into
# the shared frontends dir the `scanner` formula's bin/apg wrapper points at.

class ApgGo < Formula
  desc "Go scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.29.1",
      revision: "d28f4053484532cdd284cf009fe1410441a41276"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.29.1"
    rebuild 9
    sha256 cellar: :any_skip_relocation, arm64_sequoia: "1de54acbabdd6fcbc6e51a4f7e9272e6e8068dd7c4d3602c1f8fd9d9a8b9f3fe"
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
