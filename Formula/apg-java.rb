# Java scanner frontend for apg. Compiles CallGraphBuilder.class and drops the
# classes dir into the shared frontends dir the `scanner` formula points at.

class ApgJava < Formula
  desc "Java scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.21.0",
      revision: "22e3df4f6607e9f09b3e9357c1a32bf3ac6d25ab"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.21.0"
    rebuild 65
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "b6589892110151031d70553963a66db0b88325253b4c11b163dcefd759f9e198"
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
