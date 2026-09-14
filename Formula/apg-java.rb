# Java scanner frontend for apg. Compiles CallGraphBuilder.class and drops the
# classes dir into the shared frontends dir the `scanner` formula points at.

class ApgJava < Formula
  desc "Java scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.13.1",
      revision: "24e2c49d7f998ed78475c15e419e0506d1403fcf"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.13.1"
    rebuild 27
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "e7e179ff64978869d0670edb9b684be472b338fd9885af5dac3b490950724113"
  end

  depends_on "openjdk" # javac to build the frontend; java at runtime
  depends_on "scanner"

  def install
    mkdir "java-classes"
    system "javac",
           "-d", "java-classes",
           "-proc:none",
           # Target Java 17 bytecode so the compiled frontend runs on any JVM
           # >= 17 regardless of the JDK that compiled it (the formula builds
           # with brew's latest openjdk, but users run it with `java` on PATH).
           # `--release` is incompatible with --add-exports on jdk.compiler, so
           # -source/-target is used instead.
           "-source", "17", "-target", "17",
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
