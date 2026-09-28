# Go scanner frontend for apg. Builds the gofrontend binary and drops it into
# the shared frontends dir the `scanner` formula's bin/apg wrapper points at.

class ApgGo < Formula
  desc "Go scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.19.0",
      revision: "a017aba469e19f0c3e6abe222b25448f92043be1"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.19.0"
    rebuild 53
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "4db1fdb27bb83d2eb6e17f30fc42b6e74eb1ad65fb648547b2eef0e029115574"
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
