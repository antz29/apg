# C++ scanner frontend for apg. Compiles the tree-sitter-based scanner and
# drops the binary into the shared frontends dir the `scanner` formula points
# at. Mirrors the gcc/g++ steps in build.rs.

class ApgCpp < Formula
  desc "C++ scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.26.0",
      revision: "b0f52dc66d5ba2059ffa20e6e24a629673971ae0"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.26.0"
    rebuild 83
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "993ce780f88af61a7c8f81e21f51b4a1968e55450aafa5e9613f60ba09be115b"
  end

  depends_on "scanner"

  def install
    (buildpath/"tmp").mkdir
    cpplib = buildpath/"src/cpplib"
    vendor = cpplib/"vendor"
    ts_inc = vendor/"tree-sitter/lib/include"
    ts_src = vendor/"tree-sitter/lib/src"
    cpp_inc = vendor/"tree-sitter-cpp/src"
    args = ["-fPIC", "-std=c11"]

    system ENV.cc, *args,
           "-I#{ts_inc}", "-I#{ts_src}",
           "#{ts_src}/lib.c", "-c", "-o", "tmp/ts_runtime.o"
    system ENV.cc, *args,
           "-I#{cpp_inc}", "-I#{ts_inc}",
           "#{cpp_inc}/parser.c", "-c", "-o", "tmp/ts_cpp_parser.o"
    system ENV.cc, *args,
           "-I#{cpp_inc}", "-I#{ts_inc}",
           "#{cpp_inc}/scanner.c", "-c", "-o", "tmp/ts_cpp_scanner.o"
    system ENV.cxx, "-fPIC", "-std=c++17",
           "-I#{ts_inc}", "-I#{cpp_inc}",
           "#{cpplib}/main.cpp", "-c", "-o", "tmp/cppfrontend_main.o"
    system ENV.cxx,
           "tmp/ts_runtime.o", "tmp/ts_cpp_parser.o", "tmp/ts_cpp_scanner.o",
           "tmp/cppfrontend_main.o", "-lm", "-o", "cppfrontend"

    (share/"apg/frontends").install "cppfrontend"
  end

  test do
    assert_path_exists share/"apg/frontends/cppfrontend"
  end
end
