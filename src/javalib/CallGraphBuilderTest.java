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
 *   java --add-exports jdk.compiler/com.sun.source.tree=ALL-UNNAMED ...
 *        CallGraphBuilderTest
 */
public class CallGraphBuilderTest {
    static int failures = 0;

    public static void main(String[] args) throws Exception {
        Path base = Files.createTempDirectory("apg-java-test");
        try {
            Path proj = base.resolve("proj");
            writeFixture(proj);
            testFullScanEqualsAllTargets(proj, base);
            testTargetedEqualsFullForTargetPackage(proj, base);
            testWarmCacheReusesUnchangedPackages(proj, base);
            testEmptyTargetsMeansNoFilter(proj, base);
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

        Set<String> fullForTarget = filterToTarget(normalize(fullRaw), "pkg.b", bFile(proj));
        Set<String> inc = normalize(incRaw);
        check("targeted facts for pkg.b equal the full scan's pkg.b facts",
            fullForTarget.equals(inc), setDiff(fullForTarget, inc) + "\nSTDERR:\n" + incR.err);
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

    /** An empty --targets file means NO filter. */
    static void testEmptyTargetsMeansNoFilter(Path proj, Path base) throws Exception {
        String full = run(proj).out;
        Path targets = base.resolve("empty.targets");
        Files.writeString(targets, "", StandardCharsets.UTF_8);
        String out = run(proj, "--targets", targets.toString()).out;
        check("an empty targets file is byte-identical to the full scan",
            full.equals(out), diff(full, out));
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

    /** Keeps only the records a targeted scan of `targetPkg`/`targetFile` should emit. */
    static Set<String> filterToTarget(Set<String> all, String targetPkg, Path targetFile) {
        String tf = targetFile.toString();
        Set<String> targetUnits = new TreeSet<>();
        Set<String> targetModules = new TreeSet<>();
        for (String s : all) {
            String[] p = s.split("\\|", -1);
            if ((p[0].equals("struct") || p[0].equals("function")) && p[2].equals(tf)) targetUnits.add(p[1]);
        }
        targetModules.add(targetPkg);
        int dot = targetPkg.lastIndexOf('.');
        while (dot > 0) {
            targetModules.add(targetPkg.substring(0, dot));
            dot = targetPkg.lastIndexOf('.', dot - 1);
        }
        Set<String> keep = new TreeSet<>();
        Set<String> keptUnresolved = new TreeSet<>();
        for (String s : all) {
            String[] p = s.split("\\|", -1);
            switch (p[0]) {
                case "module" -> { if (targetModules.contains(p[1])) keep.add(s); }
                case "file" -> { if (p[1].equals(tf)) keep.add(s); }
                case "struct", "function" -> { if (p[2].equals(tf)) keep.add(s); }
                case "contains" -> {
                    if ((targetUnits.contains(p[1]) || targetModules.contains(p[1]))
                        && (targetUnits.contains(p[2]) || targetModules.contains(p[2]))) keep.add(s);
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

    static void deleteRec(Path p) throws Exception {
        if (!Files.exists(p)) return;
        try (var walk = Files.walk(p)) {
            walk.sorted(Comparator.reverseOrder()).forEach(x -> {
                try { Files.deleteIfExists(x); } catch (Exception e) { /* ignore */ }
            });
        }
    }
}
