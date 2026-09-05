# Java scanner frontend for apg. Compiles CallGraphBuilder.class and drops the
# classes dir into the shared frontends dir the `scanner` formula points at.

class ApgJava < Formula
  desc "Java scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.8.0",
      revision: "939efde7e92a581dd00a27df8dcc07a0e6a5437e"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.8.0"
    rebuild 8
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "bdbdb2dad6774980d6afe69adeeda8e329d2fd5ff6b698cf8376789999e2bcf0"
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
