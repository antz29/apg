import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.*;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Gate tests for the phase-02 task-11 targeted (package-granularity) scan.
 *
 * The load-bearing invariant: a targeted scan's facts for the target packages
 * must equal what a full scan emits for them (the requirement
 * `requirements.requirement.exact-ongoing-equivalence` and
 * `global.constraint.frontend-full-context`). Plus: an all-targets request and
 * an absent/empty target file are byte-identical to a full scan, and a warm
 * class-cache scan reuses unchanged packages instead of recompiling them.
 *
 * Run (from src/javalib after CallGraphBuilder.java is compiled):
 *   java CallGraphBuilderTest
 */
public class CallGraphBuilderTest {
    static int failures = 0;

    public static void main(String[] args) throws Exception {
        // Phase-04: the pure module-identity helpers — no temp dir, no javac,
        // no filesystem: run before any real-I/O case.
        testModuleIdentityHelpers();
        Path base = Files.createTempDirectory("apg-java-test");
        try {
            Path proj = base.resolve("proj");
            writeFixture(proj);
            testFullScanEqualsAllTargets(proj, base);
            testTargetedEqualsFullForTargetPackage(proj, base);
            testWarmCacheReusesUnchangedPackages(proj, base);
            testFullScanSeedsClassCache(proj, base);
            testSurfaceFromSourceRecoversDroppedDeclarations(proj);
            testEmptyTargetsMeansNoFilter(proj, base);
            testNoMatchingTargetsEmitsScaffoldingOnly(proj, base);
            Path proj2 = base.resolve("proj-incomplete");
            writeIncompleteContextFixture(proj2);
            testIncompleteClassDirStillResolvesTargetPackage(proj2, base);
            Path proj3 = base.resolve("proj-modules");
            writeModuleDescriptorFixture(proj3);
            testModuleDescriptorsDoNotDegradeAttribution(proj3, base);
            testDefaultPackageModuleIdentity(base);
            testDefaultPackageTargetedScaffolding(base);
        } finally {
            deleteRec(base);
        }
        if (failures == 0) {
            System.out.println("ALL TESTS PASSED");
        } else {
            System.out.println(failures + " TEST(S) FAILED");
            System.exit(1);
        }
    }

    // ------------------------------------------------------------------
    // Fixture: pkg.a (unchanged) is referenced from pkg.b (target).
    // ------------------------------------------------------------------
    static void writeFixture(Path root) throws Exception {        Files.createDirectories(root.resolve("pkg/a"));
        Files.createDirectories(root.resolve("pkg/b"));
        Files.createDirectories(root.resolve("pkg/c"));
        Files.writeString(root.resolve("pkg/a/A.java"), """
            package pkg.a;
            public class A {
                public A() {}
                public int leaf() { return 1; }
                public int over(int x) { return x; }
                public int over() { return 0; }
            }
            """, StandardCharsets.UTF_8);
        // C has an IMPLICIT constructor: the full scan emits an unresolved_call
        // to pkg.a.C.<init> (the default ctor is never declared), and the
        // targeted scan must reproduce that exactly.
        Files.writeString(root.resolve("pkg/a/C.java"), """
            package pkg.a;
            public class C {
                public int c() { return 2; }
            }
            """, StandardCharsets.UTF_8);
        // B is the target: it calls into the unchanged pkg.a,
        // cross-package, with explicit + implicit constructors and overloads.
        Files.writeString(root.resolve("pkg/b/B.java"), """
            package pkg.b;
            import pkg.a.A;
            import pkg.a.C;
            public class B {
                public int foo() {
                    A a = new A();
                    C c = new C();
                    return a.leaf() + c.c();
                }
                public int ov(A a) {
                    return a.over(1) + a.over();
                }
            }
            """, StandardCharsets.UTF_8);
        // CC is unchanged and CALLS the target: its edges must come from the
        // reused cache, never from the target scan.
        Files.writeString(root.resolve("pkg/c/CC.java"), """
            package pkg.c;
            import pkg.b.B;
            public class CC {
                public int bar() { return new B().foo(); }
            }
            """, StandardCharsets.UTF_8);
    }

    static Path bFile(Path proj) {
        return proj.resolve("pkg/b/B.java").toAbsolutePath().normalize();
    }

    /**
     * Phase-04 task-15 fixture: the Maven-like nested-source-root defect class.
     *
     * Sources sit under a MAVEN-LIKE NESTED root (`src/main/java/pkg/...`), so
     * the scan root is NOT a valid package root. That is the jgrapht shape
     * (`<root>/jgrapht-core/src/main/java/org/jgrapht/...`) and it is what makes
     * the old `-sourcepath <scan root>` silently INEFFECTIVE — javac looks for
     * `<scan root>/pkg/a/A.java`, which does not exist, so a reference into a
     * non-target package degrades to a javac error symbol. Sources placed
     * directly under the scan root (the previous fixture) never reproduced the
     * divergence because the scan root WAS a valid package root there.
     *
     * `pkg.a.Broken` is a source javac cannot compile — it references a package
     * that does not exist — so javac refuses to emit ANY bytecode for the batch
     * and the class dir is left empty (the `-classpath` half of the context is
     * empty too). The re-emitted `pkg.b` therefore has NO usable context for
     * the unchanged `pkg.a`: the cross-package reference collapses to a bare
     * project-class simple name (`A`), the call degrades to a bare method name
     * (`foo`), and the exact `calls` edge vanishes. The reference to the
     * TARGET-package `pkg.b.Target` resolves because it is compiled in the same
     * batch.
     */
    static void writeIncompleteContextFixture(Path root) throws Exception {
        Path src = root.resolve("src/main/java");
        Files.createDirectories(src.resolve("pkg/a"));
        Files.createDirectories(src.resolve("pkg/b"));
        Files.createDirectories(src.resolve("pkg/c"));
        Files.writeString(src.resolve("pkg/a/A.java"), """
            package pkg.a;
            public class A {
                public A() {}
                public int foo() { return 1; }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(src.resolve("pkg/a/Broken.java"), """
            package pkg.a;
            public class Broken {
                public missing.Thing boom() { return null; }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(src.resolve("pkg/b/Target.java"), """
            package pkg.b;
            public class Target {
                public Target() {}
                public int t() { return 2; }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(src.resolve("pkg/b/B.java"), """
            package pkg.b;
            import pkg.a.A;
            public class B {
                private final A a = new A();
                private final Target t = new Target();
                public int go() {
                    A local = a;
                    return local.foo() + t.t();
                }
                public Target make() { return new Target(); }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(src.resolve("pkg/c/C.java"), """
            package pkg.c;
            import pkg.b.B;
            public class C {
                public int baz() { return new B().go(); }
            }
            """, StandardCharsets.UTF_8);
    }

    /** The Maven-like nested source root the incomplete-context fixture uses. */
    static Path nestedSrc(Path proj) {
        return proj.resolve("src/main/java").toAbsolutePath().normalize();
    }

    /**
     * Phase-04 task-15 residual: the module-descriptor defect class.
     *
     * jgrapht is a Maven multi-module tree with a `module-info.java` per module
     * (`<root>/<module>/src/main/java/module-info.java`). Handing several module
     * descriptors to ONE javac task makes javac report `too many module
     * declarations found` and attribute every source inside a NAMED module,
     * whose `requires` graph does not read `java.desktop` / `java.sql` /
     * `java.xml` / `org.xml.sax`. Every type from those JDK modules then
     * degrades to a javac error symbol, so the FULL-scan leg emits bare simple
     * names (`JFrame`, `pack`, `setVisible`, `add`, `AttributesImpl`,
     * `StreamResult`, ...) while the TARGETED leg — whose target batch carries
     * no module descriptor — resolves the same receivers exactly
     * (`javax.swing.JFrame.<init>`, `java.awt.Window.pack`,
     * `java.awt.Component.add`, ... `stdlib`). The full scan is the side losing
     * context, so it is the side this fixture pins.
     *
     * Smallest-scale reproduction: two modules, each with a `module-info.java`
     * that requires only `java.base`, and a target class using
     * `javax.swing.JFrame`. RED pre-fix (the full scan emits the bare `JFrame`
     * / `pack` / `setVisible` unknowns and diverges from the targeted leg);
     * GREEN post-fix (both legs resolve the JDK receivers and the targeted
     * facts equal the full scan's for the target package).
     */
    static void writeModuleDescriptorFixture(Path root) throws Exception {
        Path a = root.resolve("mod-a/src/main/java");
        Path b = root.resolve("mod-b/src/main/java");
        Files.createDirectories(a.resolve("pkg/a"));
        Files.createDirectories(b.resolve("pkg/b"));
        Files.writeString(a.resolve("module-info.java"),
            "module mod.a {\n    requires java.base;\n}\n", StandardCharsets.UTF_8);
        Files.writeString(a.resolve("pkg/a/A.java"), """
            package pkg.a;
            public class A {
                public A() {}
                public int foo() { return 1; }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(b.resolve("module-info.java"),
            "module mod.b {\n    requires java.base;\n}\n", StandardCharsets.UTF_8);
        Files.writeString(b.resolve("pkg/b/Demo.java"), """
            package pkg.b;
            import pkg.a.A;
            public class Demo {
                public int go() {
                    A a = new A();
                    javax.swing.JFrame frame = new javax.swing.JFrame();
                    frame.pack();
                    frame.setVisible(true);
                    return a.foo();
                }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(b.resolve("pkg/b/Target.java"), """
            package pkg.b;
            public class Target {
                public Target() {}
                public int t() { return 2; }
            }
            """, StandardCharsets.UTF_8);
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    /** An all-targets request is byte-identical to a plain full scan. */
    static void testFullScanEqualsAllTargets(Path proj, Path base) throws Exception {
        String full = run(proj).out;
        Path targets = base.resolve("all.targets");
        StringBuilder sb = new StringBuilder();
        try (var walk = Files.walk(proj)) {
            for (Path p : (Iterable<Path>) walk.filter(x -> x.toString().endsWith(".java"))::iterator) {
                sb.append(p.toAbsolutePath().normalize()).append('\n');
            }
        }
        Files.writeString(targets, sb.toString(), StandardCharsets.UTF_8);
        Path cache = base.resolve("cache-all");
        String targeted = run(proj, "--targets", targets.toString(),
            "--cache-dir", cache.toString(), "--cache-key", "k1").out;
        check("all-targets scan is byte-identical to the full scan",
            full.equals(targeted), diff(full, targeted));
    }

    /** The target package's facts equal the full scan's facts for that package. */
    static void testTargetedEqualsFullForTargetPackage(Path proj, Path base) throws Exception {
        String fullRaw = run(proj).out;
        Path targets = base.resolve("b.targets");
        Files.writeString(targets, bFile(proj) + "\n", StandardCharsets.UTF_8);
        Path cache = base.resolve("cache-b");
        Result incR = run(proj, "--targets", targets.toString(),
            "--cache-dir", cache.toString(), "--cache-key", "k1");
        String incRaw = incR.out;

        Set<String> fullForTarget = filterToTarget(normalize(fullRaw), bFile(proj));
        Set<String> inc = normalize(incRaw);
        check("targeted facts for pkg.b equal the full scan's pkg.b facts",
            fullForTarget.equals(inc), setDiff(fullForTarget, inc) + "\nSTDERR:\n" + incR.err);

        // feedback-103: global Module->Module scaffolding is NOT per-file
        // emission. The targeted scan walks every source file (to build the
        // class cache and package map), so the unchanged packages' module
        // records and hierarchy edges must be present even though only pkg.b's
        // per-file facts are re-emitted. Java was the lone frontend omitting
        // this, which broke DB/export == full-rebuild equivalence on a partial
        // scan that still SPAWNS the Java frontend.
        check("targeted scan carries the unchanged packages' module records",
            inc.contains("module|pkg") && inc.contains("module|pkg.a") && inc.contains("module|pkg.c"),
            "targeted output was:\n" + incRaw);
        check("targeted scan carries the unchanged packages' Module->Module contains edges",
            inc.contains("contains|pkg|pkg.a") && inc.contains("contains|pkg|pkg.c"),
            "targeted output was:\n" + incRaw);
    }

    /** A second scan with the same tree must not recompile the unchanged packages. */
    static void testWarmCacheReusesUnchangedPackages(Path proj, Path base) throws Exception {
        Path targets = base.resolve("b2.targets");
        Files.writeString(targets, bFile(proj) + "\n", StandardCharsets.UTF_8);
        Path cache = base.resolve("cache-warm");
        Result r1 = run(proj, "--targets", targets.toString(),
            "--cache-dir", cache.toString(), "--cache-key", "k1");
        Result r2 = run(proj, "--targets", targets.toString(),
            "--cache-dir", cache.toString(), "--cache-key", "k1");
        check("cold targeted scan compiles the unchanged packages",
            r1.err.contains("unchanged-package"), "stderr was:\n" + r1.err);
        check("warm targeted scan reuses the class cache (no recompile)",
            !r2.err.contains("unchanged-package"), "stderr was:\n" + r2.err);
        check("warm targeted scan emits the same facts",
            normalize(r1.out).equals(normalize(r2.out)), diff(r1.out, r2.out));
    }

    /**
     * Phase-04 task-17/-32: a cold full scan invoked with the pinned
     * --cache-dir/--cache-key hand-off persists the SAME class dir + surface the
     * targeted path consumes, so the FIRST targeted scan after it recompiles no
     * unchanged packages and still emits the full scan's facts. Without both
     * flags no artifact is written (today's behaviour).
     */
    static void testFullScanSeedsClassCache(Path proj, Path base) throws Exception {
        String full = run(proj).out;

        // A flag absent either way writes no cache artifact.
        Path halfCache = base.resolve("cache-half");
        run(proj, "--cache-dir", halfCache.toString());
        check("full scan without --cache-key writes no cache artifact",
            !Files.exists(halfCache.resolve("java")), "unexpected artifact under " + halfCache);

        // Cold full scan WITH both flags seeds <cache-dir>/java/<cache-key>/.
        Path cache = base.resolve("cache-seed");
        Result seeded = run(proj, "--cache-dir", cache.toString(), "--cache-key", "k1");
        Path javaRoot = cache.resolve("java").resolve("k1");
        check("full scan with both flags writes the class dir + surface",
            Files.isDirectory(javaRoot.resolve("classes")) && Files.exists(javaRoot.resolve("surface.tsv")),
            "contents of " + javaRoot + ": " + listRec(javaRoot));
        check("a seeded full scan's facts are byte-identical to a plain full scan",
            full.equals(seeded.out), diff(full, seeded.out));

        // The first targeted scan after the seed reuses it: no recompile.
        Path targets = base.resolve("seed-b.targets");
        Files.writeString(targets, bFile(proj) + "\n", StandardCharsets.UTF_8);
        Result r = run(proj, "--targets", targets.toString(),
            "--cache-dir", cache.toString(), "--cache-key", "k1");
        check("first targeted scan after the cold full scan recompiles no unchanged package",
            !r.err.contains("unchanged-package"), "stderr was:\n" + r.err);
        Set<String> fullForTarget = filterToTarget(normalize(full), bFile(proj));
        check("seeded targeted scan's pkg.b facts still equal the full scan's",
            fullForTarget.equals(normalize(r.out)),
            setDiff(fullForTarget, normalize(r.out)) + "\nSTDERR:\n" + r.err);
    }

    /**
     * Phase-04 task-31: a source javac drops from bytecode generation still
     * contributes its declaration surface — recovered from a parse-only pass,
     * so the file is never silently absent from the surface/class context.
     */
    static void testSurfaceFromSourceRecoversDroppedDeclarations(Path proj) {
        CallGraphBuilder.FileRec rec = CallGraphBuilder.surfaceFromSource(bFile(proj));
        check("surfaceFromSource recovers the dropped source's struct declarations",
            rec != null && rec.structs.contains("pkg.b.B"),
            "rec structs: " + (rec == null ? "null" : rec.structs));
        check("surfaceFromSource recovers the dropped source's class flats",
            rec != null && rec.flats.contains("pkg.b.B"),
            "rec flats: " + (rec == null ? "null" : rec.flats));
        check("surfaceFromSource records the dropped source's content hash",
            rec != null && !rec.hash.isEmpty(), "rec hash: " + (rec == null ? "null" : rec.hash));
    }

    /** An empty --targets file means NO filter. */
    static void testEmptyTargetsMeansNoFilter(Path proj, Path base) throws Exception {
        String full = run(proj).out;
        Path targets = base.resolve("empty.targets");
        Files.writeString(targets, "", StandardCharsets.UTF_8);
        String out = run(proj, "--targets", targets.toString()).out;
        check("an empty targets file is byte-identical to the full scan",
            full.equals(out), diff(full, out));
    }

    /**
     * Phase-04 task-15: a TARGET-package class referenced from a re-emitted file
     * resolves exactly even when (a) the sources sit under a Maven-like nested
     * root — so the scan root is NOT a valid package root and the old
     * `-sourcepath <scan root>` was silently ineffective — and (b) a source
     * javac cannot compile has left the class dir empty. This FAILS pre-fix
     * (the cross-package call is missing, a bare project-class simple name `A`
     * and a bare method name `foo` leak as UnresolvedTargets) and PASSES
     * post-fix (the `-sourcepath` names the ACTUAL source root
     * `<proj>/src/main/java`).
     *
     * The enumerated oracle is the phase-04 task-9 one, scaled down: the whole
     * target package's facts must equal a from-scratch full scan's facts for
     * those files (per-record set equality), with NO UnresolvedTarget carrying
     * a bare project-class simple name and none carrying a javac error symbol.
     */
    static void testIncompleteClassDirStillResolvesTargetPackage(Path proj, Path base) throws Exception {
        Path src = nestedSrc(proj);
        String full = run(proj).out;
        Path targets = base.resolve("incomplete-b.targets");
        Files.writeString(targets, src.resolve("pkg/b/B.java") + "\n",
            StandardCharsets.UTF_8);
        Result incR = run(proj, "--targets", targets.toString(),
            "--cache-dir", base.resolve("cache-incomplete").toString(), "--cache-key", "k1");
        Set<String> inc = normalize(incR.out);

        // The class-dir half of the context really is EMPTY: the compile batch
        // fails on Broken.java, so javac emits no bytecode at all. The exact
        // facts asserted below therefore came from the corrected SOURCEPATH,
        // not from a bytecode cache — the property this fix establishes.
        Path classes = base.resolve("cache-incomplete").resolve("java").resolve("k1").resolve("classes");
        long classFiles = 0;
        if (Files.isDirectory(classes)) {
            try (var walk = Files.walk(classes)) {
                classFiles = walk.filter(x -> x.toString().endsWith(".class")).count();
            }
        }
        check("the class dir is empty (the batch could not emit bytecode)",
            classFiles == 0, "class files under " + classes + ": " + classFiles);

        // The whole target package is re-emitted (Java's package granularity).
        Set<Path> targetFiles = Set.of(
            src.resolve("pkg/b/B.java"),
            src.resolve("pkg/b/Target.java"));
        Set<String> fullForTarget = filterToTarget(normalize(full), targetFiles);
        check("targeted facts equal the full scan's when the class dir is incomplete",
            fullForTarget.equals(inc), setDiff(fullForTarget, inc) + "\nSTDERR:\n" + incR.err);

        // The CORRECT resolved targets must appear (the pre-fix deficit).
        check("the re-emitted file's call into the unchanged package resolves",
            inc.contains("calls|pkg.b.B.go|pkg.a.A.foo"), "targeted output was:\n" + incR.out);
        check("the re-emitted file's calls to the TARGET-package class resolve",
            inc.contains("calls|pkg.b.B.go|pkg.b.Target.t")
                && inc.contains("calls|pkg.b.B.make|pkg.b.Target.<init>"),
            "targeted output was:\n" + incR.out);
        check("the re-emitted file USES the TARGET-package class",
            inc.contains("uses|pkg.b.B.make|pkg.b.Target"), "targeted output was:\n" + incR.out);

        // No bare project-class simple name, no error symbol (the javac
        // error-symbol leak note-87 measured on jgrapht).
        Set<String> projectClasses = new TreeSet<>();
        for (String line : full.split("\n")) {
            if (line.contains("\"type\":\"struct\"")) {
                String name = str(line, "name");
                if (name != null) projectClasses.add(name);
            }
        }
        List<String> bare = new ArrayList<>();
        List<String> errors = new ArrayList<>();
        for (String s : inc) {
            String[] p = s.split("\\|", -1);
            if (!p[0].equals("unresolved")) continue;
            if (projectClasses.contains(p[1])) bare.add(p[1]);
            if (p[1].contains("<error>")) errors.add(p[1]);
        }
        check("no UnresolvedTarget carries a bare project-class simple name",
            bare.isEmpty(), "bare: " + bare + "\ntargeted output was:\n" + incR.out);
        check("no UnresolvedTarget carries a javac error symbol",
            errors.isEmpty(), "errors: " + errors + "\ntargeted output was:\n" + incR.out);
    }

    /**
     * Phase-04 task-15 residual / feedback-138: the FULL-scan leg must not lose
     * JDK-module visibility to a named-module compilation, AND the module
     * descriptor must still be a SCANNED file — a `File` record for it, exactly
     * once, byte-identical on both legs (domain.entity.source-file: every
     * eligible source file is included; filtering is by code_type, never by
     * dropping the file). Both legs must resolve the JDK receivers exactly
     * (qualified `stdlib` targets, no bare `unknown` names), and the targeted
     * facts must equal the full scan's for the target package.
     */
    static void testModuleDescriptorsDoNotDegradeAttribution(Path proj, Path base) throws Exception {
        Path srcA = proj.resolve("mod-a/src/main/java").toAbsolutePath().normalize();
        Path srcB = proj.resolve("mod-b/src/main/java").toAbsolutePath().normalize();
        Path descA = srcA.resolve("module-info.java");
        Path descB = srcB.resolve("module-info.java");

        String fullRaw = run(proj).out;
        Set<String> full = normalize(fullRaw);
        check("full scan resolves the JDK constructor across module descriptors",
            full.contains("unresolved|javax.swing.JFrame.<init>|stdlib"),
            "full output was:\n" + fullRaw);
        check("full scan resolves the JDK methods across module descriptors",
            full.contains("unresolved|java.awt.Window.pack|stdlib")
                && full.contains("unresolved|java.awt.Window.setVisible|stdlib"),
            "full output was:\n" + fullRaw);
        check("full scan leaks no bare JDK simple name",
            !full.contains("unresolved|JFrame|unknown")
                && !full.contains("unresolved|pack|unknown")
                && !full.contains("unresolved|setVisible|unknown"),
            "full output was:\n" + fullRaw);

        // feedback-138: a descriptor is a WALKED file, so its `file` record is
        // emitted from the walked set (never from a compilation unit: no
        // descriptor ever joins a javac batch), exactly once per descriptor,
        // with parent "" and the same line count a compilation unit would report.
        String descARec = "file|" + descA + "||1|3";
        String descBRec = "file|" + descB + "||1|3";
        check("full scan emits a File record for each module descriptor",
            full.contains(descARec) && full.contains(descBRec),
            "missing descriptor file record(s) in:\n" + fullRaw);
        check("full scan emits each module descriptor File record exactly once",
            fileRecordCount(fullRaw, descA) == 1 && fileRecordCount(fullRaw, descB) == 1,
            "descriptor file-record counts: mod-a=" + fileRecordCount(fullRaw, descA)
                + " mod-b=" + fileRecordCount(fullRaw, descB) + "\n" + fullRaw);

        // The re-emission target set names the CHANGED module descriptor too: a
        // descriptor edit must keep its File node without the descriptor ever
        // entering an attribution batch or a -sourcepath root (the JDK
        // receivers below still resolve, which is the poisoning this guards).
        Path targets = base.resolve("modules-b.targets");
        Files.writeString(targets, srcB.resolve("pkg/b/Demo.java") + "\n"
            + descB + "\n", StandardCharsets.UTF_8);
        Result incR = run(proj, "--targets", targets.toString(),
            "--cache-dir", base.resolve("cache-modules").toString(), "--cache-key", "k1");
        Set<String> inc = normalize(incR.out);
        Set<Path> targetFiles = Set.of(
            srcB.resolve("pkg/b/Demo.java"),
            srcB.resolve("pkg/b/Target.java"),
            descB);
        Set<String> fullForTarget = filterToTarget(full, targetFiles);
        check("targeted facts equal the full scan's across module descriptors",
            fullForTarget.equals(inc), setDiff(fullForTarget, inc) + "\nSTDERR:\n" + incR.err);
        check("targeted scan also resolves the JDK receivers",
            inc.contains("unresolved|javax.swing.JFrame.<init>|stdlib")
                && inc.contains("unresolved|java.awt.Window.pack|stdlib"),
            "targeted output was:\n" + incR.out + "\nSTDERR:\n" + incR.err);
        check("targeted scan emits the requested module descriptor File record exactly once",
            fileRecordCount(incR.out, descB) == 1 && inc.contains(descBRec),
            "descriptor file-record count: " + fileRecordCount(incR.out, descB)
                + "\n" + incR.out + "\nSTDERR:\n" + incR.err);
        check("targeted scan does not emit the non-target module descriptor",
            fileRecordCount(incR.out, descA) == 0,
            "targeted output was:\n" + incR.out);
    }

    /**
     * A target list naming only paths outside the walked tree (e.g. deleted
     * sources) selects no per-file facts, but it is still a SPAWNED Java
     * frontend: it must carry every walked package's global Module->Module
     * scaffolding, exactly as a full scan does (feedback-103).
     */
    static void testNoMatchingTargetsEmitsScaffoldingOnly(Path proj, Path base) throws Exception {
        Path targets = base.resolve("missing.targets");
        Files.writeString(targets, base.resolve("gone/G.java").toAbsolutePath().normalize() + "\n",
            StandardCharsets.UTF_8);
        String out = run(proj, "--targets", targets.toString()).out;
        Set<String> recs = normalize(out);
        check("no-match targets emit the global module scaffolding",
            recs.containsAll(Set.of("module|pkg", "module|pkg.a", "module|pkg.b", "module|pkg.c")),
            "targeted output was:\n" + out);
        check("no-match targets emit the Module->Module hierarchy edges",
            recs.containsAll(Set.of("contains|pkg|pkg.a", "contains|pkg|pkg.b", "contains|pkg|pkg.c")),
            "targeted output was:\n" + out);
        boolean perFile = recs.stream().anyMatch(s -> s.startsWith("file|") || s.startsWith("struct|")
            || s.startsWith("function|"));
        check("no-match targets emit no per-file facts", !perFile, "targeted output was:\n" + out);
    }

    // ------------------------------------------------------------------
    // Phase-04: default-package module identity
    // ------------------------------------------------------------------

    /**
     * The extracted String helpers are pure and side-effect free (no temp dir,
     * no javac, no filesystem): the empty (default) package yields a non-empty
     * module identity; the File parent and a top-level class parent are that
     * same identity; the class fqn has no leading dot; and packaged inputs
     * (`pkg.b` / `B`) render exactly as they did before the extraction.
     */
    static void testModuleIdentityHelpers() {
        String def = CallGraphBuilder.moduleIdentityFor("");
        check("default package yields a non-empty module identity",
            def != null && !def.isEmpty(), "identity: " + def);
        check("a packaged module identity passes through unchanged",
            "pkg.b".equals(CallGraphBuilder.moduleIdentityFor("pkg.b")),
            "got: " + CallGraphBuilder.moduleIdentityFor("pkg.b"));
        check("the File parent of the default package is the module identity",
            def.equals(CallGraphBuilder.fileParentFor("")),
            "got: " + CallGraphBuilder.fileParentFor(""));
        check("the File parent of a packaged input is unchanged",
            "pkg.b".equals(CallGraphBuilder.fileParentFor("pkg.b")),
            "got: " + CallGraphBuilder.fileParentFor("pkg.b"));
        check("a default-package top-level class parent is the module identity",
            def.equals(CallGraphBuilder.classParentFor("", "")),
            "got: " + CallGraphBuilder.classParentFor("", ""));
        check("a default-package top-level class fqn has no leading dot",
            CallGraphBuilder.classFqnFor("", "B").equals("B")
                && !CallGraphBuilder.classFqnFor("", "B").startsWith("."),
            "got: " + CallGraphBuilder.classFqnFor("", "B"));
        check("a default-package nested class keeps the enclosing class as parent",
            "Outer".equals(CallGraphBuilder.classParentFor("", "Outer")),
            "got: " + CallGraphBuilder.classParentFor("", "Outer"));
        check("packaged top-level class parent and fqn are unchanged",
            "pkg.b".equals(CallGraphBuilder.classParentFor("pkg.b", ""))
                && "pkg.b.B".equals(CallGraphBuilder.classFqnFor("pkg.b", "B")),
            "parent: " + CallGraphBuilder.classParentFor("pkg.b", "")
                + " fqn: " + CallGraphBuilder.classFqnFor("pkg.b", "B"));
        check("packaged nested class parent is unchanged",
            "pkg.a.Outer".equals(CallGraphBuilder.classParentFor("pkg.a", "Outer")),
            "got: " + CallGraphBuilder.classParentFor("pkg.a", "Outer"));
    }

    /**
     * Phase-04 e2e (CallGraphBuilderTest harness): a scratch DEFAULT-PACKAGE
     * fixture (no `package` declaration) run through the real scanner. The
     * package-less File must hang under a non-empty default-package module and
     * the top-level class must render with a module parent — never a
     * leading-dot fqn.
     *
     * The `java` Language root itself is materialised by the Rust ingestor,
     * one Language node per language that emits at least one `module` record
     * (`src/ingest.rs`: `for language in &languages`). This harness drives
     * CallGraphBuilder.main directly (no ingestor), so it pins the frontend
     * fact that makes the root exist: the non-empty default-package module
     * record. The realised `java` Language node + Module->File->Struct subtree
     * is asserted by the candidate-binary e2e in `src/main.rs`.
     */
    static void testDefaultPackageModuleIdentity(Path base) throws Exception {
        Path proj = base.resolve("proj-default-pkg");
        Files.createDirectories(proj);
        Files.writeString(proj.resolve("Widget.java"), """
            public class Widget {
                public int size() { return 1; }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(proj.resolve("Gadget.java"), """
            class Gadget {
                static class Inner { int v() { return 2; } }
            }
            """, StandardCharsets.UTF_8);
        Path widget = proj.resolve("Widget.java").toAbsolutePath().normalize();
        Path gadget = proj.resolve("Gadget.java").toAbsolutePath().normalize();

        String def = CallGraphBuilder.moduleIdentityFor("");
        String raw = run(proj).out;
        Set<String> recs = normalize(raw);

        check("default-package scan emits the non-empty default-package module record",
            recs.contains("module|" + def), "records were:\n" + raw);
        check("the package-less Files hang under the default-package module",
            recs.contains("file|" + widget + "|" + def + "|1|3")
                && recs.contains("file|" + gadget + "|" + def + "|1|3"),
            "records were:\n" + raw);
        check("no File record renders an empty module parent",
            !raw.contains("\"type\":\"file\",\"path\":\"" + widget + "\",\"parent\":\"\"")
                && !raw.contains("\"type\":\"file\",\"path\":\"" + gadget + "\",\"parent\":\"\""),
            "records were:\n" + raw);
        check("the top-level class parent is the default-package module",
            structFqn(recs, widget, def + ".Widget") != null, "records were:\n" + raw);
        check("no class record renders a leading-dot fqn",
            noLeadingDotStruct(raw, widget) && noLeadingDotStruct(raw, gadget),
            "records were:\n" + raw);
        check("a nested class keeps its enclosing class as parent",
            structFqn(recs, gadget, "Gadget.Inner") != null, "records were:\n" + raw);
    }

    /**
     * Phase-04 task-12 e2e (CallGraphBuilderTest harness): the
     * targeted/incremental branch must carry the default package's module
     * record exactly as a full scan does — feedback-103's invariant that a
     * spawned frontend emits every walked package's scaffolding.
     *
     * `proj-default-mixed` pairs an UNCHANGED default-package source with a
     * target packaged source: the default package is then in neither the
     * re-emitted set (it is not a target) nor any Module->Module hierarchy edge
     * (a single-segment identity has none), so its `(default)` module record
     * can only come from the global scaffolding. Pre-fix the scaffolding set
     * dropped the empty package and the targeted module set was a strict subset
     * of the full scan's; the no-match-targets branch dropped it too.
     */
    static void testDefaultPackageTargetedScaffolding(Path base) throws Exception {
        Path proj = base.resolve("proj-default-mixed");
        Files.createDirectories(proj.resolve("pkg/b"));
        Files.writeString(proj.resolve("Root.java"), """
            class Root {
                int r() { return 0; }
            }
            """, StandardCharsets.UTF_8);
        Files.writeString(proj.resolve("pkg/b/B.java"), """
            package pkg.b;
            public class B {
                public int foo() { return 1; }
            }
            """, StandardCharsets.UTF_8);
        Path rootFile = proj.resolve("Root.java").toAbsolutePath().normalize();
        Path bFile = proj.resolve("pkg/b/B.java").toAbsolutePath().normalize();
        String def = CallGraphBuilder.moduleIdentityFor("");

        Set<String> full = normalize(run(proj).out);
        check("full scan emits the default-package module alongside the packaged ones",
            modulesOf(full).equals(new TreeSet<>(Set.of(def, "pkg", "pkg.b"))),
            "full modules: " + modulesOf(full));

        // Normal targeted branch: only pkg.b is a target; Root.java's default
        // package is unchanged and arrives solely via global scaffolding.
        Path targets = base.resolve("default-targeted.targets");
        Files.writeString(targets, bFile + "\n", StandardCharsets.UTF_8);
        Result incR = run(proj, "--targets", targets.toString(),
            "--cache-dir", base.resolve("cache-default-targeted").toString(), "--cache-key", "k1");
        Set<String> inc = normalize(incR.out);
        check("targeted scan with an unchanged default package emits its module record",
            inc.contains("module|" + def), "targeted output was:\n" + incR.out);
        check("targeted scan's module set equals the full scan's",
            modulesOf(inc).equals(modulesOf(full)),
            "full modules: " + modulesOf(full) + "\ntargeted modules: " + modulesOf(inc)
                + "\n" + incR.out);
        check("targeted scan does not re-emit the non-target default-package file",
            !inc.contains("file|" + rootFile + "|"), "targeted output was:\n" + incR.out);
        check("targeted scan still re-emits the target package's File record",
            inc.contains("file|" + bFile + "|pkg.b|1|4"), "targeted output was:\n" + incR.out);

        // No-match-targets branch: a non-empty target list naming no walked
        // source still scaffolds every walked package, default included.
        Path gone = base.resolve("default-gone.targets");
        Files.writeString(gone, base.resolve("gone/G.java").toAbsolutePath().normalize() + "\n",
            StandardCharsets.UTF_8);
        String out = run(proj, "--targets", gone.toString()).out;
        Set<String> recs = normalize(out);
        check("no-match targeted scan emits the default-package module record",
            recs.contains("module|" + def), "targeted output was:\n" + out);
        check("no-match targeted scan also emits the packaged scaffolding",
            recs.contains("module|pkg") && recs.contains("module|pkg.b"),
            "targeted output was:\n" + out);
    }

    // ------------------------------------------------------------------
    // Scanner invocation
    // ------------------------------------------------------------------

    static final class Result {
        final String out;
        final String err;
        Result(String out, String err) { this.out = out; this.err = err; }
    }

    static Result run(Path proj, String... extra) throws Exception {
        List<String> args = new ArrayList<>();
        args.add(proj.toString());
        args.addAll(Arrays.asList(extra));
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        ByteArrayOutputStream err = new ByteArrayOutputStream();
        PrintStream oldOut = System.out, oldErr = System.err;
        System.setOut(new PrintStream(out, true, StandardCharsets.UTF_8));
        System.setErr(new PrintStream(err, true, StandardCharsets.UTF_8));
        try {
            CallGraphBuilder.main(args.toArray(new String[0]));
        } finally {
            System.setOut(oldOut);
            System.setErr(oldErr);
        }
        return new Result(out.toString(StandardCharsets.UTF_8), err.toString(StandardCharsets.UTF_8));
    }

    // ------------------------------------------------------------------
    // Record normalization
    // ------------------------------------------------------------------

    static String str(String line, String key) {
        Matcher m = Pattern.compile("\"" + key + "\":\"((?:[^\"\\\\]|\\\\.)*)\"").matcher(line);
        if (!m.find()) return null;
        String s = m.group(1);
        return s.replace("\\\"", "\"").replace("\\\\", "\\").replace("\\n", "\n");
    }

    static String num(String line, String key) {
        Matcher m = Pattern.compile("\"" + key + "\":(-?\\d+)").matcher(line);
        return m.find() ? m.group(1) : "";
    }

    static List<String> params(String line) {
        Matcher m = Pattern.compile("\"params\":\\[(.*?)\\]").matcher(line);
        if (!m.find()) return List.of();
        String body = m.group(1).trim();
        if (body.isEmpty()) return List.of();
        List<String> out = new ArrayList<>();
        for (String p : body.split(",")) {
            String q = p.trim();
            if (q.startsWith("\"") && q.endsWith("\"") && q.length() >= 2) q = q.substring(1, q.length() - 1);
            out.add(q);
        }
        return out;
    }

    /** Turns a scanner JSONL stream into a set of id-free canonical records. */
    static Set<String> normalize(String raw) {
        Map<String, String> id2fqn = new HashMap<>();
        List<String[]> funcs = new ArrayList<>(); // parent,name,id,params-joined
        List<String[]> structs = new ArrayList<>(); // parent,name,id,path,start,end,sl,el
        List<String> files = new ArrayList<>();
        List<String> modules = new ArrayList<>();
        List<String[]> edges = new ArrayList<>(); // type,from,to
        List<String[]> unres = new ArrayList<>(); // fqn,category

        for (String line : raw.split("\n")) {
            if (line.isEmpty()) continue;
            String type = str(line, "type");
            if (type == null) continue;
            switch (type) {
                case "module" -> modules.add("module|" + str(line, "fqn"));
                case "file" -> files.add("file|" + str(line, "path") + "|" + str(line, "parent")
                        + "|" + num(line, "start_line") + "|" + num(line, "end_line"));
                case "struct" -> {
                    String id = str(line, "id");
                    String fqn = str(line, "parent") + "." + str(line, "name");
                    id2fqn.put(id, fqn);
                    structs.add(new String[]{fqn, str(line, "path"), num(line, "start"),
                            num(line, "end"), num(line, "start_line"), num(line, "end_line")});
                }
                case "function" -> funcs.add(new String[]{str(line, "parent"), str(line, "name"),
                        str(line, "id"), String.join(",", params(line)), str(line, "file"),
                        num(line, "start"), num(line, "end"), num(line, "start_line"),
                        num(line, "end_line")});
                case "contains", "calls", "uses", "unresolved_call", "unresolved_use" ->
                        edges.add(new String[]{type, str(line, "from"), str(line, "to")});
                case "unresolved" -> unres.add(new String[]{str(line, "fqn"), str(line, "category")});
                default -> { }
            }
        }
        // Function FQNs follow the ingestor's rendering: singleton -> parent.name,
        // an overloaded (parent,name) group -> parent.name(params).
        Map<String, List<String[]>> groups = new LinkedHashMap<>();
        for (String[] f : funcs) groups.computeIfAbsent(f[0] + "\u0000" + f[1], k -> new ArrayList<>()).add(f);
        Set<String> out = new TreeSet<>();
        out.addAll(modules);
        out.addAll(files);
        for (String[] s : structs) {
            out.add("struct|" + s[0] + "|" + s[1] + "|" + s[2] + "|" + s[3] + "|" + s[4] + "|" + s[5]);
        }
        for (List<String[]> g : groups.values()) {
            for (String[] f : g) {
                String fqn = g.size() == 1 ? f[0] + "." + f[1] : f[0] + "." + f[1] + "(" + f[3] + ")";
                id2fqn.put(f[2], fqn);
                out.add("function|" + fqn + "|" + f[4] + "|" + f[5] + "|" + f[6] + "|" + f[7] + "|" + f[8]
                        + "|" + f[3]);
            }
        }
        for (String[] e : edges) {
            String from = id2fqn.getOrDefault(e[1], e[1]);
            String to = id2fqn.getOrDefault(e[2], e[2]);
            out.add(e[0] + "|" + from + "|" + to);
        }
        for (String[] u : unres) out.add("unresolved|" + u[0] + "|" + u[1]);
        return out;
    }

    /**
     * The records a targeted scan of `targetFile` must emit (feedback-103).
     *
     * Per-file facts — the target file's `file`/`struct`/`function` records and
     * the `contains`/`calls`/`uses`/`unresolved_*` edges between its units — are
     * filtered to the target file. The global Module->Module scaffolding is NOT
     * per-file emission: a targeted scan walks every source file (to build the
     * class cache and package map), so it carries every walked package's
     * `module` record and `Module->Module` contains edge, exactly as a full scan
     * does. (The target package's own scaffolding arrives via its re-emitted
     * files; the unchanged packages' arrives via the global pre-emission.)
     */
    static Set<String> filterToTarget(Set<String> all, Path targetFile) {
        return filterToTarget(all, Set.of(targetFile));
    }

    /** The same filter over a SET of target files (Java re-emits whole packages). */
    static Set<String> filterToTarget(Set<String> all, Set<Path> targetFiles) {
        Set<String> tfs = new TreeSet<>();
        for (Path p : targetFiles) tfs.add(p.toString());
        Set<String> targetUnits = new TreeSet<>();
        Set<String> modules = new TreeSet<>();
        for (String s : all) {
            String[] p = s.split("\\|", -1);
            if (p[0].equals("module")) modules.add(p[1]);
            if ((p[0].equals("struct") || p[0].equals("function")) && tfs.contains(p[2])) targetUnits.add(p[1]);
        }
        Set<String> keep = new TreeSet<>();
        Set<String> keptUnresolved = new TreeSet<>();
        for (String s : all) {
            String[] p = s.split("\\|", -1);
            switch (p[0]) {
                case "module" -> keep.add(s);
                case "file" -> { if (tfs.contains(p[1])) keep.add(s); }
                case "struct", "function" -> { if (tfs.contains(p[2])) keep.add(s); }
                case "contains" -> {
                    // Module->Module hierarchy is global; struct containment
                    // (nested struct / struct->method) is per-file.
                    if ((modules.contains(p[1]) && modules.contains(p[2]))
                        || (targetUnits.contains(p[1]) && targetUnits.contains(p[2]))) keep.add(s);
                }
                case "calls", "uses", "unresolved_call", "unresolved_use" -> {
                    if (targetUnits.contains(p[1])) {
                        keep.add(s);
                        if (p[0].startsWith("unresolved")) keptUnresolved.add(p[2]);
                    }
                }
                default -> { }
            }
        }
        Set<String> unres = new TreeSet<>();
        for (String s : all) {
            String[] p = s.split("\\|", -1);
            if (p[0].equals("unresolved") && keptUnresolved.contains(p[1])) unres.add(s);
        }
        keep.addAll(unres);
        return keep;
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    static void check(String name, boolean ok, String detail) {
        if (ok) {
            System.out.println("PASS: " + name);
        } else {
            failures++;
            System.out.println("FAIL: " + name);
            System.out.println(detail);
        }
    }

    static String setDiff(Set<String> a, Set<String> b) {
        Set<String> onlyA = new TreeSet<>(a); onlyA.removeAll(b);
        Set<String> onlyB = new TreeSet<>(b); onlyB.removeAll(a);
        return "only in full-for-target:\n" + String.join("\n", onlyA)
            + "\nonly in targeted:\n" + String.join("\n", onlyB);
    }

    static String diff(String a, String b) {
        Set<String> sa = new TreeSet<>(Arrays.asList(a.split("\n")));
        Set<String> sb = new TreeSet<>(Arrays.asList(b.split("\n")));
        return setDiff(sa, sb);
    }

    /**
     * Counts the raw `file` records for an absolute path in a scanner stream —
     * the exactly-once check a Set-based {@link #normalize} cannot make (it
     * collapses duplicates).
     */
    static int fileRecordCount(String raw, Path p) {
        String key = "\"type\":\"file\",\"path\":\"" + p.toAbsolutePath().normalize() + "\"";
        int n = 0;
        for (int i = raw.indexOf(key); i >= 0; i = raw.indexOf(key, i + 1)) n++;
        return n;
    }

    /**
     * The normalized `struct` record fqn for `path` when it renders exactly
     * `fqn` (`parent.name`), else null — the raw-scanner identity a
     * leading-dot regression would break.
     */
    static String structFqn(Set<String> recs, Path path, String fqn) {
        for (String s : recs) {
            String[] p = s.split("\\|", -1);
            if (p[0].equals("struct") && p[2].equals(path.toString()) && p[1].equals(fqn)) return p[1];
        }
        return null;
    }

    /** The set of module fqns carried by a normalized record set. */
    static Set<String> modulesOf(Set<String> recs) {
        Set<String> out = new TreeSet<>();
        for (String s : recs) {
            String[] p = s.split("\\|", -1);
            if (p[0].equals("module")) out.add(p[1]);
        }
        return out;
    }

    /** True when no `struct` record for `path` renders a leading-dot fqn. */
    static boolean noLeadingDotStruct(String raw, Path path) {
        String key = "\"type\":\"struct\"";
        for (int i = raw.indexOf(key); i >= 0; i = raw.indexOf(key, i + 1)) {
            int end = raw.indexOf('\n', i);
            String line = raw.substring(i, end < 0 ? raw.length() : end);
            if (!line.contains("\"path\":\"" + path + "\"")) continue;
            String parent = str(line, "parent");
            String name = str(line, "name");
            if (parent == null || name == null || (parent + "." + name).startsWith(".")) return false;
        }
        return true;
    }

    static String listRec(Path p) throws Exception {
        if (!Files.exists(p)) return "<absent>";
        List<String> names = new ArrayList<>();
        try (var walk = Files.walk(p)) {
            walk.forEach(x -> names.add(p.relativize(x).toString()));
        }
        Collections.sort(names);
        return String.join(", ", names);
    }

    static void deleteRec(Path p) throws Exception {
        if (!Files.exists(p)) return;
        try (var walk = Files.walk(p)) {
            walk.sorted(Comparator.reverseOrder()).forEach(x -> {
                try { Files.deleteIfExists(x); } catch (Exception e) { /* ignore */ }
            });
        }
    }
}
