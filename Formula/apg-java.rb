# Java scanner frontend for apg. Compiles CallGraphBuilder.class and drops the
# classes dir into the shared frontends dir the `scanner` formula points at.

class ApgJava < Formula
  desc "Java scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.23.0",
      revision: "0772de837c504561db695d5797fe4a2ac4fbd72f"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.23.0"
    rebuild 72
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "2572df1451d2403a0d54e2af55b772dd5f85a003b5c4a60d69135519401634c0"
  end

  depends_on "openjdk" # javac to build the frontend; java at runtime
  depends_on "scanner"

  def install
    mkdir "java-classes"
    # Compile the whole non-test Java source set: the frontend is a set of
    # sibling default-package classes (CallGraphBuilder plus its collaborators),
    # so javac must be handed every source file — listing CallGraphBuilder.java
    # alone would not find a sibling class. The test class is excluded: it is
    # not part of the shipped frontend.
    sources = Dir["src/javalib/*.java"].reject { |f| f.end_with?("CallGraphBuilderTest.java") }.sort
    system "javac",
           "-d", "java-classes",
           "-proc:none",
           # Target Java 21 bytecode and compile against the Java 21 public API
           # only, so the compiled frontend runs on any JVM >= 21 regardless of
           # the JDK that compiled it. The frontend uses no JDK-internal APIs,
           # so `--release` (not -source/-target + --add-exports) is what keeps
           # the build JDK's internals out of the artifact.
           "--release", "21",
           *sources
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
