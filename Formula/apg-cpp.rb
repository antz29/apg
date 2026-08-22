# C++ scanner frontend for apg. Compiles the tree-sitter-based scanner and
# drops the binary into the shared frontends dir the `scanner` formula points
# at. Mirrors the gcc/g++ steps in build.rs.

class ApgCpp < Formula
  desc "C++ scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.6.1",
      revision: "bc8ca897227bfc457b302f386c6b252a75f4bd8c"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.6.1"
    rebuild 4
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "2aa58032ee655e98595436fc788d6d070f8ad6ddeda3abf53355489fff95ce44"
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
