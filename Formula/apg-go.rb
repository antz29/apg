# Go scanner frontend for apg. Builds the gofrontend binary and drops it into
# the shared frontends dir the `scanner` formula's bin/apg wrapper points at.

class ApgGo < Formula
  desc "Go scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg/archive/refs/tags/v0.1.0.tar.gz"
  # sha256 "<FILL_ME — run `curl -sL <url> | shasum -a 256` after tagging v0.1.0>"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

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
