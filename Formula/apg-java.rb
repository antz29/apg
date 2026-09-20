# Java scanner frontend for apg. Compiles CallGraphBuilder.class and drops the
# classes dir into the shared frontends dir the `scanner` formula points at.

class ApgJava < Formula
  desc "Java scanner frontend for apg"
  homepage "https://github.com/antz29/apg"
  url "https://github.com/antz29/apg.git",
      tag:      "v0.15.2",
      revision: "14aadba4fe15e86853a0f5197f1ad9c5b8addb3a"
  license "MIT"
  head "https://github.com/antz29/apg.git", branch: "main"

  bottle do
    root_url "https://github.com/antz29/apg/releases/download/v0.15.2"
    rebuild 41
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "3deb568329ce1420e28425d3febafabed0e8cbb4d5e652a2add6fed08d18716e"
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
