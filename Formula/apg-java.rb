# Java scanner frontend for apg. Compiles CallGraphBuilder.class and drops the
# classes dir into the shared frontends dir the `scanner` formula points at.

class ApgJava < Formula
  desc "Java scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.6.0",
      revision: "99eb1b5d828d157db307ec4ca8ee65f6749235e3"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.6.0"
    rebuild 4
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "f19bb9cfa1af93f7b7efefdc55a6c04292904f0964ded24c54ef5a9709e34f65"
  end

  depends_on "openjdk" # javac to build the frontend; java at runtime
  depends_on "scanner"

  def install
    mkdir "java-classes"
    system "javac",
           "-d", "java-classes",
           "-proc:none",
           "--add-exports", "jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED",
           "--add-exports", "jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
           "--add-exports", "jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED",
           "--add-exports", "jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED",
           "src/javalib/CallGraphBuilder.java"
    (share/"apg/frontends").install "java-classes"
  end

  def caveats
    <<~EOS
      Scanning Java projects needs `java` on your PATH. Since openjdk is
      keg-only, either link it or export:

        export PATH="#{formula_opt_bin("openjdk")}:$PATH"
    EOS
  end

  test do
    assert_path_exists share/"apg/frontends/java-classes/CallGraphBuilder.class"
  end
end
