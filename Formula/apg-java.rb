# Java scanner frontend for apg. Compiles CallGraphBuilder.class and drops the
# classes dir into the shared frontends dir the `scanner` formula points at.

class ApgJava < Formula
  desc "Java scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.17.0",
      revision: "3c55e42b5e233fbdfec5026e59be50bf0c348624"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.17.0"
    rebuild 51
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "c57983758aad5e16fed3a9197e5bb947b39aa92aac8d79d236a806e4bb7f47cd"
  end

  depends_on "openjdk" # javac to build the frontend; java at runtime
  depends_on "scanner"

  def install
    mkdir "java-classes"
    system "javac",
           "-d", "java-classes",
           "-proc:none",
           # Target Java 21 bytecode and compile against the Java 21 public API
           # only, so the compiled frontend runs on any JVM >= 21 regardless of
           # the JDK that compiled it. The frontend uses no JDK-internal APIs,
           # so `--release` (not -source/-target + --add-exports) is what keeps
           # the build JDK's internals out of the artifact.
           "--release", "21",
           "src/javalib/CallGraphBuilder.java"
    (share/"apg/frontends").install "java-classes"
  end

  def caveats
    <<~EOS
      Scanning Java projects needs `java` (JDK 21 or newer) on your PATH. Since
      openjdk is keg-only, either link it or export:

        export PATH="#{formula_opt_bin("openjdk")}:$PATH"
    EOS
  end

  test do
    assert_path_exists share/"apg/frontends/java-classes/CallGraphBuilder.class"
  end
end
