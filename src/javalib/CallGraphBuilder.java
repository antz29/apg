import com.sun.source.tree.*;
import com.sun.source.util.*;
import com.sun.tools.javac.code.Symbol;
import com.sun.tools.javac.code.Type;
import com.sun.tools.javac.tree.JCTree;
import javax.tools.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.*;
public class CallGraphBuilder {
    static final long START = System.currentTimeMillis();

    static String elapsed() {
        long s = (System.currentTimeMillis() - START) / 1000;
        return String.format("%d:%02d", s / 60, s % 60);
    }

    /** Basename of a compilation unit's source file. */
    static String fileBase(CompilationUnitTree u) {
        String p = u.getSourceFile().toUri().getPath();
        int s = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
        return s >= 0 ? p.substring(s + 1) : p;
    }

    /**
     * One newline-terminated progress line per unit, on stderr. stderr goes to
     * a log file (apg-frontend.log), so this is safe to make verbose: it never
     * streams into the terminal/tool.
     */
    static void progress(String label, int done, int total, String item) {
        int pct = total == 0 ? 100 : done * 100 / total;
        System.err.println("[" + elapsed() + "] " + label + " " + pct + "% (" + done + "/" + total + ")"
            + (item.isEmpty() ? "" : " " + item));
        System.err.flush();
    }

    static void endProgress() {
        System.err.println();
        System.err.flush();
    }

    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        // `--id-prefix <p>` (default "n") keeps opaque ids unique across
        // frontends when a scan merges multiple languages. The three
        // phase-02 task-11 hand-off flags are appended beside it on the
        // incremental path (the pinned target-set interface).
        String idPrefix = "n";
        List<String> excludePaths = new ArrayList<>();
        String targetsPath = null;
        String cacheDir = null;
        String cacheKey = null;
        for (int i = 1; i < args.length; i++) {
            if (args[i].equals("--id-prefix") && i + 1 < args.length) {
                idPrefix = args[i + 1];
                i++;
            } else if (args[i].equals("--targets") && i + 1 < args.length) {
                targetsPath = args[i + 1];
                i++;
            } else if (args[i].equals("--cache-dir") && i + 1 < args.length) {
                cacheDir = args[i + 1];
                i++;
            } else if (args[i].equals("--cache-key") && i + 1 < args.length) {
                cacheKey = args[i + 1];
                i++;
            } else {
                excludePaths.add(args[i]);
            }
        }
        String prefix = idPrefix;

        System.err.println("[" + elapsed() + "] collecting source files...");
        List<Path> files = new ArrayList<>();
        // Discovery walk. Prune hidden directories — any directory whose
        // basename begins with '.' — notably a nested project worktree under
        // `apg/.worktrees/`. Walking into it compiles the parent checkout's
        // sources and the worktree's copy together, which fails with
        // `duplicate class: CallGraphBuilder`. The other frontends all prune
        // hidden names (domain.entity.scan-exclusion: `.worktrees/` is never a
        // scan root); this brings Java to the same rule. The scan root itself
        // is exempt, so a scan root that lives under `.worktrees/` is still
        // scanned.
        Files.walkFileTree(dir, new SimpleFileVisitor<Path>() {
            @Override
            public FileVisitResult preVisitDirectory(Path d, BasicFileAttributes attrs) {
                Path name = d.getFileName();
                if (!d.equals(dir) && name != null && name.toString().startsWith(".")) {
                    return FileVisitResult.SKIP_SUBTREE;
                }
                return FileVisitResult.CONTINUE;
            }

            // Phase-04 task-15 residual (feedback-138): a `module-info.java`
            // is a module DESCRIPTOR — it declares no package and no
            // struct/function/edge facts — but it is still a SCANNED source
            // file, so it stays in the walked set and the graph still carries
            // its `file` record (domain.entity.source-file: every eligible
            // source file is included; filtering is by code_type, never by
            // dropping the file). It is partitioned OUT of every javac task and
            // every `-sourcepath` root instead (partitionDescriptors): ONE
            // descriptor in a batch flips the whole compilation into
            // NAMED-module mode, whose `requires` graph hides every JDK module
            // the descriptor does not read
            // (java.desktop/java.sql/java.xml/org.xml.sax/...), and each such
            // type then degrades to an error symbol whose calls/uses leak out as
            // bare simple names (`JFrame`, `pack`, `add`, `StreamResult`). A
            // Maven multi-module tree carries one per module (jgrapht), so a
            // one-task full scan reported `too many module declarations found`
            // and lost JDK visibility wholesale. The scanner attributes an
            // UNNAMED-module source tree (global.constraint.frontend-full-
            // context: resolve against the FULL context), so descriptors are
            // attributed nowhere on BOTH legs while remaining part of the
            // scanned file set.
            @Override
            public FileVisitResult visitFile(Path p, BasicFileAttributes attrs) {
                if (!attrs.isRegularFile()) return FileVisitResult.CONTINUE;
                if (!p.toString().endsWith(".java")) return FileVisitResult.CONTINUE;
                if (excludePaths.stream().anyMatch(pat -> p.toString().contains(pat))) {
                    return FileVisitResult.CONTINUE;
                }
                files.add(p);
                return FileVisitResult.CONTINUE;
            }
        });
        System.err.println("[" + elapsed() + "] " + files.size() + " .java files");

        // The target set is an EMISSION filter only (phase-02 task-11). An
        // absent flag or an empty file means NO filter — the byte-identical
        // full-scan path below. A non-empty list in force selects its packages
        // for re-emission (a list matching no walked file emits nothing).
        List<Path> targets = null;
        if (targetsPath != null) {
            targets = readTargetList(Paths.get(targetsPath));
            if (targets.isEmpty()) targets = null;
        }
        if (targets == null) {
            // Phase-04 task-32: the pinned --cache-dir/--cache-key hand-off
            // reaches the full-scan path too (task-17 seeds the class cache);
            // the incremental dispatch and the emission filter are unchanged.
            runFullScan(dir, files, prefix, cacheDir, cacheKey);
        } else {
            runIncrementalScan(dir, files, targets, prefix, cacheDir, cacheKey);
        }
    }

    /** True for a `module-info.java` module descriptor. */
    static boolean isModuleDescriptor(Path p) {
        return "module-info.java".equals(p.getFileName().toString());
    }

    /**
     * Splits a walked set into attributable sources and module descriptors.
     *
     * A descriptor is part of the WALKED/scanned file set — its `file` record
     * is a graph fact (feedback-138; domain.entity.source-file: every eligible
     * source file is included, filtering is by code_type, never by dropping the
     * file) — but it must never reach a javac task or a `-sourcepath` root: one
     * descriptor in a batch flips javac into NAMED-module attribution (`too
     * many module declarations found`), whose `requires` graph hides every JDK
     * module the descriptor does not read, degrading each such receiver to a
     * bare simple name. Both legs call this before building any batch, so the
     * descriptor's `file` record is always emitted from the walked set (see
     * Collector.emitWalkedFile) and never from a compilation unit.
     */
    static void partitionDescriptors(List<Path> files, List<Path> sources, List<Path> descriptors) {
        for (Path f : files) {
            if (isModuleDescriptor(f)) descriptors.add(f);
            else sources.add(f);
        }
    }

    /**
     * The full-context scan: parse everything, attribute everything, emit
     * everything. When the pinned `--cache-dir`/`--cache-key` hand-off is
     * present it also seeds the native class cache (phase-04 task-17) so the
     * next targeted scan reuses it instead of recompiling every unchanged
     * source.
     */
    static void runFullScan(Path dir, List<Path> files, String prefix, String cacheDir, String cacheKey)
            throws Exception {
        // Module descriptors stay in the walked set (their `file` record is a
        // graph fact) but are attributed nowhere; every batch/total/surface
        // below is over the attributable sources only.
        List<Path> sources = new ArrayList<>();
        List<Path> descriptors = new ArrayList<>();
        partitionDescriptors(files, sources, descriptors);
        // Snapshot the attributable sources before crashing-file isolation
        // mutates `sources`: the seeded surface must be complete over every
        // attributable input (a descriptor declares nothing to surface).
        List<Path> allFiles = new ArrayList<>(sources);
        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        var fm = compiler.getStandardFileManager(null, null, null);

        // Parse everything first (declarations are collected/emitted from the
        // parse tree; edges are resolved against attributed symbols).
        var task = newTask(compiler, fm, sources);
        int total = sources.size();
        var units = new ArrayList<CompilationUnitTree>();
        int i = 0;
        for (CompilationUnitTree unit : task.parse()) {
            units.add(unit);
            i++;
            progress("Parsing", i, total, fileBase(unit));
        }
        endProgress();

        // Attribute for exact call/type resolution. javac can crash on a few
        // files (e.g. switch-expression AssertionError on JDK 17); isolate and
        // drop those files, then re-attribute the rest so the graph keeps
        // exact edges everywhere else.
        List<Path> crashing = new ArrayList<>();
        if (!tryAnalyze(task, total)) {
            System.err.println("WARNING: attribution crashed; isolating offending files...");
            crashing = findCrashingFiles(compiler, fm, sources);
            System.err.println();
            System.err.println("WARNING: excluding " + crashing.size() + " files from attribution: " + crashing);
            sources.removeAll(crashing);
            System.err.println("[" + elapsed() + "] re-parsing and re-attributing " + sources.size() + " remaining files...");
            task = newTask(compiler, fm, sources);
            units.clear();
            for (var unit : task.parse()) units.add(unit);
            tryAnalyze(task, total);
        }

        System.err.println("[" + elapsed() + "] pass 1: assigning ids to declared classes and methods...");
        var c = new Collector(prefix);
        // Pass 1: assign opaque ids to every declared class and method.
        c.collectAll(units);
        System.err.println("[" + elapsed() + "] pass 2: emitting nodes and edges...");
        // Pass 2: emit node + edge records, resolving endpoints by id.
        c.emitAll(units, total);
        // The walked module descriptors: a `file` record each, byte-identical to
        // a compilation unit's, emitted from the walked set exactly once (they
        // are in no attributed batch, so no unit ever emits them). feedback-138.
        for (Path d : descriptors) c.emitWalkedFile(d);
        c.flush();

        // Phase-04 task-17: stdout is complete; seed the native class cache the
        // targeted path consumes. Best-effort — a cache failure never fails the
        // scan, and an absent flag writes nothing.
        seedClassCache(allFiles, dir, cacheDir, cacheKey);
    }

    /**
     * The pinned native-artifact root `<cache-dir>/java/<cache-key>`, or null
     * when either flag is absent. The full scan only persists when both flags
     * are present: an absent flag keeps the pre-phase-04 behaviour and writes
     * no artifact (no cache path is fabricated or defaulted).
     */
    static Path persistentJavaRoot(String cacheDir, String cacheKey) {
        if (cacheDir == null || cacheDir.isEmpty()) return null;
        if (cacheKey == null || cacheKey.isEmpty()) return null;
        return Paths.get(cacheDir, "java").resolve(cacheKey);
    }

    /**
     * Phase-04 task-17: persists the SAME class dir + declaration surface the
     * targeted path consumes (`compileAndCollect` + `saveSurface`), so the first
     * targeted scan after a cold full scan reuses it and reports zero
     * `compiling N unchanged-package file(s)`. A full scan has every source, so
     * it is authoritative (global.constraint.frontend-full-context); a partial
     * surface is never persisted.
     */
    static void seedClassCache(List<Path> allFiles, Path dir, String cacheDir, String cacheKey) {
        Path javaRoot = persistentJavaRoot(cacheDir, cacheKey);
        if (javaRoot == null) return;
        Path root = dir.toAbsolutePath().normalize();
        try {
            Path classesDir = javaRoot.resolve("classes");
            Files.createDirectories(classesDir);
            List<String> classOpts = new ArrayList<>(List.of(
                    "-classpath", classesDir.toString(),
                    "-d", classesDir.toString()));
            Set<Path> wanted = new HashSet<>();
            for (Path f : allFiles) wanted.add(f.toAbsolutePath().normalize());
            System.err.println("[" + elapsed() + "] seeding java class cache for "
                + wanted.size() + " file(s) into " + javaRoot);
            Map<Path, FileRec> raw = new LinkedHashMap<>();
            var compiler = ToolProvider.getSystemJavaCompiler();
            var fm = compiler.getStandardFileManager(null, null, null);
            compileAndCollect(compiler, fm, new ArrayList<>(allFiles), classOpts, raw, wanted);
            // The surface the targeted path consumes is keyed relative to the
            // scan root (runIncrementalScan does `root.resolve(rel)`).
            Map<String, FileRec> surface = new LinkedHashMap<>();
            for (var e : raw.entrySet()) surface.put(relOf(root, e.getKey()), e.getValue());
            if (surface.size() != wanted.size()) {
                System.err.println("WARNING: java class surface incomplete ("
                    + surface.size() + "/" + wanted.size()
                    + " file(s) surfaced); not persisting a partial surface");
                return;
            }
            saveSurface(javaRoot.resolve("surface.tsv"), surface);
            System.err.println("[" + elapsed() + "] persisted java class surface for "
                + surface.size() + " file(s)");
        } catch (Throwable t) {
            System.err.println("WARNING: could not seed java class cache: " + t);
        }
    }

    // ------------------------------------------------------------------
    // Phase-02 task-11: package-granularity targeted (incremental) scan.
    //
    // The target set is an EMISSION filter only: the changed packages are
    // compiled/attributed from source while the unchanged packages are made
    // available as BYTECODE on a shared class dir, so javac resolves every
    // dependency exactly without re-analyzing the unchanged sources from
    // scratch. Only the target packages' facts are emitted; edges into an
    // unchanged package are emitted against its canonical FQN, which the
    // ingestor's cached-fact splice resolves (the unchanged file's unit is
    // reused, so its node is present).
    // ------------------------------------------------------------------

    /** Reads the `--targets` list: absolute source paths, one per line, blanks ignored. */
    static List<Path> readTargetList(Path file) {
        List<Path> out = new ArrayList<>();
        try {
            for (String line : Files.readAllLines(file, StandardCharsets.UTF_8)) {
                String s = line.trim();
                if (s.isEmpty()) continue;
                out.add(Paths.get(s).toAbsolutePath().normalize());
            }
        } catch (IOException e) {
            System.err.println("WARNING: could not read targets " + file + ": " + e);
            return new ArrayList<>();
        }
        return out;
    }

    /** Per-source declaration surface persisted in the class cache. */
    static final class FileRec {
        String hash = "";
        boolean compiled = false;
        final List<String> structs = new ArrayList<>();
        final List<String> funcs = new ArrayList<>();
        final List<String> flats = new ArrayList<>();
    }

    static void runIncrementalScan(Path dir, List<Path> allFiles, List<Path> targetList,
            String prefix, String cacheDir, String cacheKey) throws Exception {
        Path root = dir.toAbsolutePath().normalize();

        // The native artifact lives at <cache-dir>/java/<cache-key>/ (pinned).
        Path javaRoot = null;
        if (cacheDir != null && !cacheDir.isEmpty()) {
            javaRoot = Paths.get(cacheDir, "java");
            if (cacheKey != null && !cacheKey.isEmpty()) javaRoot = javaRoot.resolve(cacheKey);
        }
        boolean persistent = javaRoot != null;
        Path classesDir;
        Path surfaceFile;
        if (persistent) {
            Files.createDirectories(javaRoot);
            classesDir = javaRoot.resolve("classes");
            surfaceFile = javaRoot.resolve("surface.tsv");
        } else {
            classesDir = Files.createTempDirectory("apg-java-classes");
            surfaceFile = null;
        }
        Files.createDirectories(classesDir);

        // A `module-info.java` must never enter a javac batch (target,
        // non-target, compileAndCollect) nor a `-sourcepath` root: one
        // descriptor flips attribution into NAMED-module mode and hides the JDK
        // modules it does not read. It is still a WALKED/scanned file, though,
        // so it is carried separately for its `file` record — and never grouped
        // by package, where its empty package (`pkgByFile` -> "") would pull it
        // into the default package's target/non-target batches (feedback-138).
        List<Path> sourceList = new ArrayList<>();
        List<Path> descriptorList = new ArrayList<>();
        partitionDescriptors(allFiles, sourceList, descriptorList);

        // Caller-provided target paths are absolute; map them onto the walked
        // tree. Sources are the attribution/re-emission units; a descriptor can
        // only ever select its own `file` record (it belongs to no package), so
        // it is matched separately and never joins targetFiles/nonTargetFiles.
        Map<Path, Path> walked = new LinkedHashMap<>();
        for (Path f : sourceList) walked.putIfAbsent(f.toAbsolutePath().normalize(), f);
        Set<Path> descriptorByAbs = new LinkedHashSet<>();
        for (Path f : descriptorList) descriptorByAbs.add(f.toAbsolutePath().normalize());
        Set<Path> requested = new LinkedHashSet<>();
        Set<Path> requestedDescriptors = new LinkedHashSet<>();
        for (Path t : targetList) {
            Path a = t.toAbsolutePath().normalize();
            if (walked.containsKey(a)) requested.add(a);
            else if (descriptorByAbs.contains(a)) requestedDescriptors.add(a);
        }
        // Package of every walked source file (the re-emission unit for Java).
        // Parsing is cheap; attribution is the expensive part we keep to the
        // target set. Phase 02 note: java compilers/file managers carry option
        // state across tasks in one JVM, so every task gets a fresh compiler +
        // file manager.
        Map<Path, String> pkgByFile = packageMap(ToolProvider.getSystemJavaCompiler(),
                new ArrayList<>(walked.keySet()));

        if (requested.isEmpty()) {
            // A non-empty target list that matches no walked source file selects
            // no per-file facts (an explicit filter is in force), never
            // everything. It is still a SPAWNED Java frontend, though, so its
            // global Module->Module scaffolding must cover every walked package
            // (feedback-103), exactly as a full scan's does. A requested module
            // descriptor is a walked file with a `file` record but no package,
            // so it is emitted here (never attributed) alongside that
            // scaffolding.
            LinkedHashSet<String> allPkgs = new LinkedHashSet<>(pkgByFile.values());
            allPkgs.remove("");
            System.err.println("[" + elapsed() + "] targets matched no attributable scanned file; "
                + "emitting global module scaffolding for " + allPkgs.size() + " package(s)"
                + (requestedDescriptors.isEmpty() ? " only"
                    : " + " + requestedDescriptors.size() + " module descriptor file(s)"));
            var c = new Collector(prefix, true, Set.of(), Map.of());
            c.emitGlobalPkgScaffolding(allPkgs);
            for (Path d : requestedDescriptors) c.emitWalkedFile(d);
            c.flush();
            return;
        }

        Set<String> targetPkgs = new HashSet<>();
        for (Path t : requested) targetPkgs.add(pkgByFile.getOrDefault(t, ""));
        Set<Path> targetFiles = new LinkedHashSet<>();
        Set<Path> nonTargetFiles = new LinkedHashSet<>();
        for (Path f : walked.keySet()) {
            if (targetPkgs.contains(pkgByFile.getOrDefault(f, ""))) targetFiles.add(f);
            else nonTargetFiles.add(f);
        }
        System.err.println("[" + elapsed() + "] incremental: " + targetFiles.size()
            + " target file(s) in " + targetPkgs.size() + " package(s); "
            + nonTargetFiles.size() + " reused from the class cache");

        // Load the compiled-declaration surface persisted from earlier scans.
        Map<String, FileRec> surface = persistent ? loadSurface(surfaceFile) : new HashMap<>();

        // Drop records (and their bytecode) for files that are now targets or
        // gone; compile only the non-target files whose cache entry is missing
        // or stale (content hash changed).
        List<String> staleRels = new ArrayList<>();
        for (String rel : surface.keySet()) {
            if (!nonTargetFiles.contains(root.resolve(rel))) staleRels.add(rel);
        }
        for (String rel : staleRels) {
            FileRec rec = surface.remove(rel);
            if (rec != null) deleteClasses(classesDir, rec);
        }

        Map<String, FileRec> clean = new LinkedHashMap<>();
        List<Path> dirty = new ArrayList<>();
        for (Path f : nonTargetFiles) {
            String rel = relOf(root, f);
            FileRec rec = surface.get(rel);
            String hash = sha1(f);
            if (rec != null && rec.compiled && rec.hash.equals(hash) && classesPresent(classesDir, rec)) {
                clean.put(rel, rec);
            } else {
                if (rec != null) deleteClasses(classesDir, rec);
                dirty.add(f);
            }
        }

        List<String> classOpts = new ArrayList<>(List.of(
                "-classpath", classesDir.toString(),
                "-d", classesDir.toString()));

        // Compile the dirty unchanged-package sources — plus the target files,
        // so the class dir stays a complete snapshot the unchanged packages
        // resolve against (a body-only change leaves them non-target while
        // their dependents may still reference them) — and collect the WHOLE
        // batch's declaration surface (phase-04 task-15): the target-package
        // declarations are part of the project-class index too, or the
        // emitter's guards cannot recognise a target class whose attribution
        // degraded and would leak it as a bare simple name. Crashing files are
        // isolated and dropped, exactly like the full scan.
        if (!dirty.isEmpty() || !targetFiles.isEmpty()) {
            if (!dirty.isEmpty()) {
                System.err.println("[" + elapsed() + "] compiling " + dirty.size()
                    + " unchanged-package file(s) into " + classesDir);
            }
            LinkedHashSet<Path> batch = new LinkedHashSet<>(dirty);
            batch.addAll(targetFiles);
            Map<Path, FileRec> fresh = new LinkedHashMap<>();
            var ccompiler = ToolProvider.getSystemJavaCompiler();
            var cfm = ccompiler.getStandardFileManager(null, null, null);
            compileAndCollect(ccompiler, cfm, new ArrayList<>(batch), classOpts, fresh, batch);
            for (var e : fresh.entrySet()) clean.put(relOf(root, e.getKey()), e.getValue());
            endProgress();
        }

        // Phase-04 task-15: the project-class index must be COMPLETE over the
        // WHOLE walked tree before target attribution. `compileAndCollect`
        // drops a source that crashes javac (task-31 recovers its
        // declarations), so re-check every walked file — TARGET files included,
        // whatever side of the target boundary it is on — and surface any still
        // absent from the class cache from source. A dropped source's
        // declarations must never be silently missing, or its references would
        // leak into the target attribution as bare project-class simple names /
        // error symbols.
        for (Path f : walked.keySet()) {
            String rel = relOf(root, f);
            if (clean.containsKey(rel)) continue;
            FileRec rec = surfaceFromSource(f);
            if (rec != null) {
                System.err.println("  [" + elapsed() + "] surfaced un-attributable file: " + rel);
                clean.put(rel, rec);
            } else {
                System.err.println("WARNING: no declaration surface for un-attributable file: " + rel);
            }
        }

        // Build the surface lookup: declared struct FQNs + function keys, with
        // the ingestor's overload rendering (singleton -> parent.name,
        // overload -> parent.name(params)).
        Set<String> surfaceStructs = new HashSet<>();
        Map<String, List<String>> funcGroups = new LinkedHashMap<>();
        for (FileRec rec : clean.values()) {
            surfaceStructs.addAll(rec.structs);
            for (String k : rec.funcs) {
                funcGroups.computeIfAbsent(groupKey(k), x -> new ArrayList<>()).add(k);
            }
        }
        Map<String, String> surfaceFuncFqn = new HashMap<>();
        for (List<String> keys : funcGroups.values()) {
            for (String k : keys) {
                surfaceFuncFqn.put(k, keys.size() == 1 ? k.substring(0, k.indexOf('(')) : k);
            }
        }

        // Attribute ONLY the target packages, with the unchanged packages on
        // the classpath: javac resolves deps from bytecode (full context,
        // exact) but the unchanged sources are not re-analyzed.
        //
        // Phase-04 task-15: the class dir is a CACHE, never the whole context.
        // javac refuses to emit bytecode for a compilation that has any error
        // (the error-stop policy cannot be overridden through the JavacTask
        // API), so a single unrelated error in the batch - an absent optional
        // dependency, a module-info naming a missing module - leaves the class
        // dir EMPTY and the target attribution degrades wholesale: every
        // cross-package type becomes an error symbol, declaration parameter
        // types lose their package (WeightCombiner, not
        // org.jgrapht.graph.WeightCombiner) and resolved calls collapse into
        // bare simple-name UnresolvedTargets. The SOURCEPATH is therefore the
        // real resolution context: a type the class dir cannot supply is
        // attributed from source, exactly as the full scan resolves it.
        //
        // Phase-04 task-15: the sourcepath must name the ACTUAL SOURCE ROOTS of
        // the walked files, NOT the scan root. In a Maven-like layout
        // (<root>/<module>/src/main/java/pkg/...) the scan root is not a valid
        // package root, so a scan-root sourcepath is silently INEFFECTIVE and
        // every reference into a non-target package degrades to a javac error
        // symbol. For each walked file, strip the declared-package path
        // (pkgByFile) off its parent directory; dedupe and ':'-join. A file in
        // the default/empty package contributes its own parent directory.
        LinkedHashSet<String> sourceRoots = new LinkedHashSet<>();
        Path rootsBase = null;
        int rootsIdx = 0;
        Map<Path, Path> linkedRoots = new HashMap<>();
        for (Path f : walked.keySet()) {
            Path p = f.getParent();
            String pkg = pkgByFile.getOrDefault(f, "");
            if (pkg != null && !pkg.isEmpty()) {
                for (int i = 0, n = pkg.split("\\.").length; i < n && p != null; i++) {
                    p = p.getParent();
                }
            }
            if (p == null) continue;
            // Phase-04 task-15 residual: a `module-info.java` at a sourcepath
            // entry root makes javac load that descriptor and attribute the whole
            // task in NAMED-module mode, whose `requires` graph hides every JDK
            // module the descriptor does not read (java.desktop / java.sql /
            // java.xml / org.xml.sax / ...). Each such type then degrades to a
            // javac error symbol — the targeted scan would resolve against a
            // REDUCED context, which global.constraint.frontend-full-context
            // forbids and which the full scan (all files explicit, module
            // descriptors excluded from the walk) does not suffer. A Maven
            // source root always carries the descriptor, so substitute an
            // equivalent root that links every child EXCEPT `module-info.java`:
            // package lookup is unchanged, javac finds no descriptor and stays
            // in the unnamed module.
            if (Files.exists(p.resolve("module-info.java"))) {
                if (rootsBase == null) rootsBase = Files.createTempDirectory("apg-java-srcroots");
                Path cached = linkedRoots.get(p);
                if (cached == null) {
                    Path linked = moduleInfoFreeRoot(p, rootsBase.resolve("r" + rootsIdx++));
                    cached = linked != null ? linked : p;
                    linkedRoots.put(p, cached);
                }
                p = cached;
            }
            sourceRoots.add(p.toString());
        }
        String sourcePath = sourceRoots.isEmpty() ? root.toString() : String.join(":", sourceRoots);

        List<Path> tfiles = new ArrayList<>(targetFiles);
        int total = tfiles.size();
        var tcompiler = ToolProvider.getSystemJavaCompiler();
        var tfm = tcompiler.getStandardFileManager(null, null, null);
        List<String> targetOpts = List.of(
                "-classpath", classesDir.toString(),
                "-sourcepath", sourcePath);
        var task = newTask(tcompiler, tfm, tfiles, targetOpts);
        var units = new ArrayList<CompilationUnitTree>();
        for (CompilationUnitTree unit : task.parse()) units.add(unit);
        if (!tryAnalyze(task, total)) {
            System.err.println("WARNING: attribution crashed; isolating offending files...");
            List<Path> crashing = findCrashingFiles(tcompiler, tfm, tfiles, targetOpts);
            System.err.println();
            System.err.println("WARNING: excluding " + crashing.size() + " files from attribution: " + crashing);
            tfiles.removeAll(crashing);
            task = newTask(tcompiler, tfm, tfiles, targetOpts);
            units.clear();
            for (var unit : task.parse()) units.add(unit);
            tryAnalyze(task, tfiles.size());
        }

        System.err.println("[" + elapsed() + "] pass 1: assigning ids to declared classes and methods...");
        var c = new Collector(prefix, true, surfaceStructs, surfaceFuncFqn);
        c.collectAll(units);
        // feedback-103: emit the global Module->Module scaffolding for the
        // unchanged packages the per-file filter leaves out. Target packages
        // get theirs from their own re-emitted files, so the union covers every
        // walked package — matching a full scan's hierarchy. The all-targets
        // path has no unchanged package and stays byte-identical to a full scan.
        LinkedHashSet<String> nonTargetPkgs = new LinkedHashSet<>();
        for (Path f : nonTargetFiles) nonTargetPkgs.add(pkgByFile.getOrDefault(f, ""));
        nonTargetPkgs.remove("");
        System.err.println("[" + elapsed() + "] emitting global module scaffolding for "
            + nonTargetPkgs.size() + " reused package(s)...");
        c.emitGlobalPkgScaffolding(nonTargetPkgs);
        System.err.println("[" + elapsed() + "] pass 2: emitting nodes and edges...");
        c.emitAll(units, total);
        // The module descriptors named by the re-emission target set: a `file`
        // record each, emitted from the walked set (they are attributed
        // nowhere), exactly once and byte-identical to the full leg's — so a
        // changed module-info.java keeps its File node without any descriptor
        // ever entering an attribution batch. A non-target descriptor is not
        // re-emitted here; its File node is reused from the cached fact unit,
        // exactly like any unchanged file. feedback-138.
        for (Path d : requestedDescriptors) c.emitWalkedFile(d);
        c.flush();

        // Persist the surface for the next scan (atomic replace).
        if (persistent) {
            Map<String, FileRec> out = new LinkedHashMap<>(clean);
            saveSurface(surfaceFile, out);
        }
        deleteRec(rootsBase);
    }

    /**
     * A sourcepath entry that resolves the same packages as `root` but exposes
     * no `module-info.java`, so javac never loads a module descriptor from the
     * source path. Returns null when the links cannot be created (the caller
     * then keeps `root` — today's behaviour).
     */
    static Path moduleInfoFreeRoot(Path root, Path tmp) {
        try {
            Files.createDirectories(tmp);
            try (var children = Files.list(root)) {
                for (Path c : (Iterable<Path>) children::iterator) {
                    if (c.getFileName().toString().equals("module-info.java")) continue;
                    Files.createSymbolicLink(tmp.resolve(c.getFileName()), c.toAbsolutePath());
                }
            }
            return tmp;
        } catch (Throwable t) {
            return null;
        }
    }

    /** Best-effort recursive delete of scan-local scratch (the linked roots). */
    static void deleteRec(Path p) {
        if (p == null) return;
        try (var walk = Files.walk(p)) {
            walk.sorted(java.util.Comparator.reverseOrder()).forEach(x -> {
                try {
                    Files.deleteIfExists(x);
                } catch (IOException e) {
                    /* best effort */
                }
            });
        } catch (Throwable t) {
            /* best effort */
        }
    }

    /** Parses every file and maps its absolute path to its package name. */
    static Map<Path, String> packageMap(JavaCompiler compiler, List<Path> files) {
        Map<Path, String> out = new LinkedHashMap<>();
        if (files.isEmpty()) return out;
        var fm = compiler.getStandardFileManager(null, null, null);
        try {
            var task = newTask(compiler, fm, files);
            for (CompilationUnitTree u : task.parse()) {
                String p = u.getPackageName() == null ? "" : u.getPackageName().toString();
                out.put(Paths.get(u.getSourceFile().toUri()).toAbsolutePath().normalize(), p);
            }
        } catch (Throwable t) {
            System.err.println("WARNING: package parse failed: " + t);
            for (Path f : files) out.putIfAbsent(f.toAbsolutePath().normalize(), "");
        }
        return out;
    }

    /**
     * Compiles a batch of sources into the class dir (bytecode generation must
     * happen even when an unrelated file has errors, so the error-stop policy
     * is OFF here), and records the declaration surface of the files in
     * `collect`. A batch that crashes javac is split (binary isolation) so one
     * bad file never discards the rest; a single crashing file is dropped,
     * matching the full scan's behaviour.
     */
    static void compileAndCollect(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, List<String> opts, Map<Path, FileRec> out,
            Set<Path> collect) {
        if (files.isEmpty()) return;
        try {
            var task = newTask(compiler, fm, files, opts, false);
            List<CompilationUnitTree> units = new ArrayList<>();
            for (var u : task.parse()) units.add(u);
            for (var u : task.analyze()) {
                /* drive attribution */
            }
            for (var u : units) {
                Path abs = Paths.get(u.getSourceFile().toUri()).toAbsolutePath().normalize();
                if (!collect.contains(abs)) continue;
                var sc = new SurfaceScanner();
                sc.scan(u, null);
                FileRec rec = new FileRec();
                rec.compiled = true;
                rec.hash = sha1(abs);
                rec.structs.addAll(sc.structs);
                rec.funcs.addAll(sc.funcs);
                rec.flats.addAll(sc.flats);
                out.put(abs, rec);
            }
            // `analyze()` only attributes; bytecode is produced by `generate()`.
            // (Collect first: desugaring mutates the attributed trees.)
            for (var g : task.generate()) {
                /* write the class files */
            }
        } catch (Throwable t) {
            if (files.size() == 1) {
                Path abs = files.get(0).toAbsolutePath().normalize();
                System.err.println("  [" + elapsed() + "] dropping un-attributable file: " + files.get(0));
                // Phase-04 task-31: a dropped source must still contribute its
                // declaration surface. If the crash happened after collection
                // (e.g. during generate()) the record is already present and is
                // kept; otherwise recover it from a parse-only pass — the file
                // must never silently vanish from a COMPLETE surface.
                if (collect.contains(abs) && !out.containsKey(abs)) {
                    FileRec rec = surfaceFromSource(files.get(0));
                    if (rec != null) {
                        out.put(abs, rec);
                    } else {
                        System.err.println("  [" + elapsed() + "] no declaration surface for dropped file: "
                            + files.get(0));
                    }
                }
                return;
            }
            int mid = files.size() / 2;
            compileAndCollect(compiler, fm, new ArrayList<>(files.subList(0, mid)), opts, out, collect);
            compileAndCollect(compiler, fm, new ArrayList<>(files.subList(mid, files.size())), opts, out, collect);
        }
    }

    /**
     * Phase-04 task-31: the declaration surface of a source javac dropped from
     * bytecode generation, recovered from a parse-only pass (a parse needs no
     * attribution, so an un-attributable file still parses). Erased parameter
     * types are unavailable without attribution, so the recovered function keys
     * carry no parameters; the struct FQNs — what the emitter's bare-name
     * fallback consults — are exact.
     */
    static FileRec surfaceFromSource(Path file) {
        try {
            var compiler = ToolProvider.getSystemJavaCompiler();
            var fm = compiler.getStandardFileManager(null, null, null);
            var task = newTask(compiler, fm, List.of(file));
            for (CompilationUnitTree u : task.parse()) {
                var sc = new SurfaceScanner();
                sc.scan(u, null);
                FileRec rec = new FileRec();
                rec.compiled = true;
                rec.hash = sha1(file.toAbsolutePath().normalize());
                rec.structs.addAll(sc.structs);
                rec.funcs.addAll(sc.funcs);
                rec.flats.addAll(sc.flats);
                return rec;
            }
        } catch (Throwable t) {
            /* fall through: no surface recoverable */
        }
        return null;
    }

    static String relOf(Path root, Path abs) {
        try {
            return root.relativize(abs.toAbsolutePath().normalize()).toString().replace('\\', '/');
        } catch (Exception e) {
            return abs.toString();
        }
    }

    static String groupKey(String funcKey) {
        int p = funcKey.indexOf('(');
        String prefix = p >= 0 ? funcKey.substring(0, p) : funcKey;
        int d = prefix.lastIndexOf('.');
        return d >= 0 ? prefix.substring(0, d) + "\u0000" + prefix.substring(d + 1) : prefix;
    }

    /** SHA-1 hex of a file's bytes (content identity, never mtime). */
    static String sha1(Path f) {
        try {
            var md = java.security.MessageDigest.getInstance("SHA-1");
            byte[] h = md.digest(Files.readAllBytes(f));
            StringBuilder sb = new StringBuilder(h.length * 2);
            for (byte b : h) sb.append(String.format("%02x", b & 0xff));
            return sb.toString();
        } catch (Exception e) {
            return "";
        }
    }

    static void deleteClasses(Path classesDir, FileRec rec) {
        for (String flat : rec.flats) {
            try {
                Files.deleteIfExists(classesDir.resolve(flat.replace('.', '/') + ".class"));
            } catch (IOException e) {
                /* best effort */
            }
        }
    }

    /** True when a cached record's bytecode is still present in the class dir. */
    static boolean classesPresent(Path classesDir, FileRec rec) {
        if (rec.flats.isEmpty()) return true;
        for (String flat : rec.flats) {
            if (Files.exists(classesDir.resolve(flat.replace('.', '/') + ".class"))) return true;
        }
        return false;
    }

    /** Loads the persisted `surface.tsv` (path -> declaration surface). */
    static Map<String, FileRec> loadSurface(Path file) {
        Map<String, FileRec> out = new LinkedHashMap<>();
        if (!Files.exists(file)) return out;
        try {
            for (String line : Files.readAllLines(file, StandardCharsets.UTF_8)) {
                if (line.isEmpty()) continue;
                String[] p = line.split("\t", -1);
                if (p.length < 3) continue;
                switch (p[0]) {
                    case "D" -> {
                        FileRec r = new FileRec();
                        r.compiled = true;
                        r.hash = p[2];
                        out.put(p[1], r);
                    }
                    case "S" -> rec(out, p[1]).structs.add(p[2]);
                    case "F" -> rec(out, p[1]).funcs.add(p[2]);
                    case "K" -> rec(out, p[1]).flats.add(p[2]);
                    default -> { }
                }
            }
        } catch (IOException e) {
            System.err.println("WARNING: could not read surface cache: " + e);
        }
        return out;
    }

    static FileRec rec(Map<String, FileRec> m, String path) {
        return m.computeIfAbsent(path, k -> new FileRec());
    }

    /** Atomically persists the declaration surface for the next scan. */
    static void saveSurface(Path file, Map<String, FileRec> surface) {
        StringBuilder sb = new StringBuilder();
        for (var e : surface.entrySet()) {
            FileRec r = e.getValue();
            if (!r.compiled) continue;
            sb.append("D\t").append(e.getKey()).append('\t').append(r.hash).append('\n');
            for (String s : r.structs) sb.append("S\t").append(e.getKey()).append('\t').append(s).append('\n');
            for (String f : r.funcs) sb.append("F\t").append(e.getKey()).append('\t').append(f).append('\n');
            for (String k : r.flats) sb.append("K\t").append(e.getKey()).append('\t').append(k).append('\n');
        }
        try {
            Path tmp = file.resolveSibling(file.getFileName() + ".tmp");
            Files.writeString(tmp, sb.toString(), StandardCharsets.UTF_8);
            try {
                Files.move(tmp, file, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } catch (Exception e) {
                Files.move(tmp, file, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException e) {
            System.err.println("WARNING: could not write surface cache: " + e);
        }
    }

    static JavacTask newTask(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> files) {
        return newTask(compiler, fm, files, List.of());
    }

    static JavacTask newTask(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> files,
            List<String> extra) {
        return newTask(compiler, fm, files, extra, true);
    }

    static JavacTask newTask(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> files,
            List<String> extra, boolean stopOnError) {
        List<String> opts = new ArrayList<>(List.of("-proc:none", "-Xlint:none", "-implicit:none"));
        if (stopOnError) opts.add("-XDshouldStopPolicyIfError=ATTR");
        opts.addAll(extra);
        return (JavacTask) compiler.getTask(null, fm, null, opts, null,
                fm.getJavaFileObjectsFromPaths(files));
    }

    /** Source file for a task event (ANALYZE fires per compilation unit). */
    static String eventFile(TaskEvent e) {
        try {
            if (e.getCompilationUnit() != null) return fileBase(e.getCompilationUnit());
        } catch (Throwable t) {
            /* fall through */
        }
        try {
            if (e.getTypeElement() != null) {
                String q = e.getTypeElement().getQualifiedName().toString();
                int s = Math.max(q.lastIndexOf('.'), q.lastIndexOf('$'));
                return s >= 0 ? q.substring(s + 1) : q;
            }
        } catch (Throwable t) {
            /* fall through */
        }
        return "";
    }

    /**
     * Attribute everything for exact call/type resolution. `task.analyze()` is
     * monolithic (it attributes all units before yielding any), so live
     * per-file progress comes from a TaskListener's ANALYZE events instead of
     * the iteration counter.
     */
    static boolean tryAnalyze(JavacTask task, int total) {
        try {
            final int[] n = {0};
            task.addTaskListener(new TaskListener() {
                @Override
                public void started(TaskEvent e) {
                    if (e.getKind() == TaskEvent.Kind.ANALYZE) {
                        n[0]++;
                        progress("Attributing", n[0], total, eventFile(e));
                    }
                }

                @Override
                public void finished(TaskEvent e) {
                    /* no-op */
                }
            });
            for (var unit : task.analyze()) {
                // Work is driven by the iteration; live progress comes from
                // the listener above.
            }
            endProgress();
            return true;
        } catch (Throwable t) {
            endProgress();
            return false;
        }
    }

    /** Binary-search the files that crash javac attribution. */
    static List<Path> findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> files) {
        System.err.println("[" + elapsed() + "] binary-searching " + files.size()
            + " files; each probe re-parses+re-attributes a chunk (slow)...");
        List<Path> crashing = new ArrayList<>();
        findCrashingFiles(compiler, fm, files, 0, files.size(), crashing);
        return crashing;
    }

    static void findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, int lo, int hi, List<Path> out) {
        if (lo >= hi) return;
        if (hi - lo == 1) {
            System.err.println("  [" + elapsed() + "] crashing file: " + files.get(lo));
            out.add(files.get(lo));
            return;
        }
        int mid = (lo + hi) / 2;
        System.err.println("[" + elapsed() + "] probing chunk [" + lo + "," + mid + ") of "
            + files.size() + " (" + (mid - lo) + " files)...");
        if (chunkCrashes(compiler, fm, files.subList(lo, mid))) {
            findCrashingFiles(compiler, fm, files, lo, mid, out);
        }
        System.err.println("[" + elapsed() + "] probing chunk [" + mid + "," + hi + ") of "
            + files.size() + " (" + (hi - mid) + " files)...");
        if (chunkCrashes(compiler, fm, files.subList(mid, hi))) {
            findCrashingFiles(compiler, fm, files, mid, hi, out);
        }
    }

    static boolean chunkCrashes(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> chunk) {
        if (chunk.isEmpty()) return false;
        try {
            var task = newTask(compiler, fm, chunk);
            for (var u : task.parse()) {}
            for (var u : task.analyze()) {}
            return false;
        } catch (Throwable t) {
            return true;
        }
    }

    /** Options-aware crash isolation (the targeted scan's classpath probe). */
    static List<Path> findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, List<String> opts) {
        System.err.println("[" + elapsed() + "] binary-searching " + files.size()
            + " files; each probe re-parses+re-attributes a chunk (slow)...");
        List<Path> crashing = new ArrayList<>();
        findCrashingFiles(compiler, fm, files, 0, files.size(), crashing, opts);
        return crashing;
    }

    static void findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, int lo, int hi, List<Path> out, List<String> opts) {
        if (lo >= hi) return;
        if (hi - lo == 1) {
            System.err.println("  [" + elapsed() + "] crashing file: " + files.get(lo));
            out.add(files.get(lo));
            return;
        }
        int mid = (lo + hi) / 2;
        System.err.println("[" + elapsed() + "] probing chunk [" + lo + "," + mid + ") of "
            + files.size() + " (" + (mid - lo) + " files)...");
        if (chunkCrashes(compiler, fm, files.subList(lo, mid), opts)) {
            findCrashingFiles(compiler, fm, files, lo, mid, out, opts);
        }
        System.err.println("[" + elapsed() + "] probing chunk [" + mid + "," + hi + ") of "
            + files.size() + " (" + (hi - mid) + " files)...");
        if (chunkCrashes(compiler, fm, files.subList(mid, hi), opts)) {
            findCrashingFiles(compiler, fm, files, mid, hi, out, opts);
        }
    }

    static boolean chunkCrashes(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> chunk, List<String> opts) {
        if (chunk.isEmpty()) return false;
        try {
            var task = newTask(compiler, fm, chunk, opts);
            for (var u : task.parse()) {}
            for (var u : task.analyze()) {}
            return false;
        } catch (Throwable t) {
            return true;
        }
    }

    /** Escapes a string for a JSON string literal. */
    static String jstr(String s) {
        if (s == null) return "";
        StringBuilder sb = new StringBuilder(s.length() + 16);
        for (int i = 0; i < s.length(); i++) {
            char ch = s.charAt(i);
            switch (ch) {
                case '\\': sb.append("\\\\"); break;
                case '"': sb.append("\\\""); break;
                case '\n': sb.append("\\n"); break;
                case '\r': sb.append("\\r"); break;
                case '\t': sb.append("\\t"); break;
                default:
                    if (ch < 0x20) sb.append(String.format("\\u%04x", (int) ch));
                    else sb.append(ch);
            }
        }
        return sb.toString();
    }

    /** Classifies an out-of-project method FQN as stdlib or external. */
    static String categoryOf(String mfqn) {
        if (mfqn == null) return "unknown";
        for (String p : new String[]{"java.", "javax.", "jdk.", "com.sun.", "sun."}) {
            if (mfqn.startsWith(p)) return "stdlib";
        }
        return "external";
    }

    /**
     * Returns the fully-qualified owning class (package.Outer.Inner) for a
     * symbol, walking up past synthetic anonymous/local classes ($-suffixed)
     * to the nearest named type.
     */
    static String ownerFqn(Symbol owner) {
        while (owner instanceof Symbol.ClassSymbol cs) {
            String qn = cs.getQualifiedName().toString();
            // Anonymous classes have an empty qualified name; walk up past them
            // (and past $-named synthetic classes) to the nearest named type.
            if (!qn.isEmpty() && qn.indexOf('$') < 0) return qn;
            owner = cs.getEnclosingElement();
        }
        return null;
    }

    /**
     * Erased, package-qualified parameter types of a method symbol, in
     * declaration order (SPEC §2.3). The same rendering is used for method
     * declarations and call targets, so overloads resolve exactly.
     */
    static List<String> paramStrings(Symbol.MethodSymbol ms) {
        List<String> out = new ArrayList<>();
        if (ms == null) return out;
        try {
            Type mt = ms.asType();
            for (Type pt : mt.getParameterTypes()) {
                out.add(typeString(pt));
            }
        } catch (Throwable t) {
            out.clear();
        }
        return out;
    }

    /** Renders a type: qualified for class types, recursed for arrays. */
    static String typeString(Type t) {
        if (t == null) return "";
        if (t instanceof com.sun.tools.javac.code.Type.ArrayType at) {
            return typeString(at.elemtype) + "[]";
        }
        Symbol ts = t.tsym;
        if (ts != null) return ts.getQualifiedName().toString();
        return t.toString();
    }

    /**
     * Resolves the fully-qualified type name from an attributed type tree.
     * Arrays recurse to their element type; parameterized types use the raw
     * type symbol. Null if the tree has no usable symbol.
     */
    static String typeFqn(Tree typeTree) {
        if (typeTree == null) return null;
        if (typeTree instanceof ArrayTypeTree at) return typeFqn(at.getType());
        JCTree jc = (JCTree) typeTree;
        if (jc.type != null && jc.type.tsym instanceof Symbol.ClassSymbol cs) {
            String qn = cs.getQualifiedName().toString();
            if (qn.indexOf('<') >= 0 || qn.contains("<error>")) return null;
            return qn;
        }
        return null;
    }

    static String typeRawName(Tree t) {
        if (t == null) return null;
        if (t instanceof IdentifierTree id) return id.getName().toString();
        if (t instanceof MemberSelectTree ms) return ms.getIdentifier().toString();
        if (t instanceof ParameterizedTypeTree pt) return typeRawName(pt.getType());
        if (t instanceof ArrayTypeTree at) return typeRawName(at.getType());
        return t.toString();
    }

    static String methodRawName(ExpressionTree sel) {
        if (sel instanceof MemberSelectTree ms) return ms.getIdentifier().toString();
        return sel.toString();
    }

    /** Symbol attributed onto a call/method-ref/new-class expression, or null. */
    static Symbol symOf(Tree t) {
        if (t instanceof JCTree.JCMethodInvocation inv) {
            ExpressionTree sel = inv.getMethodSelect();
            if (sel instanceof JCTree.JCIdent id) return id.sym;
            if (sel instanceof JCTree.JCFieldAccess fa) return fa.sym;
            return null;
        }
        if (t instanceof JCTree.JCNewClass nc) return nc.constructor;
        if (t instanceof JCTree.JCMemberReference mref) return mref.sym;
        return null;
    }

    /** Method symbol attributed onto a method declaration, or null. */
    static Symbol.MethodSymbol symOfDecl(MethodTree mt) {
        if (mt instanceof JCTree.JCMethodDecl jm) return jm.sym;
        return null;
    }

    /**
     * Collects the declaration surface of one unchanged-package source file:
     * the struct FQNs it declares (in the same `pkg.Outer.Inner` form the
     * Collector uses) and the erased function keys `parent.name(params)`.
     * Derived from the SAME tree walk as the full scan, so the keys — and the
     * implicit-constructor / synthetic-member behaviour — match exactly.
     */
    static class SurfaceScanner extends TreePathScanner<Void, Void> {
        String pkg = "", cls = "";
        final List<String> structs = new ArrayList<>();
        final List<String> funcs = new ArrayList<>();
        final List<String> flats = new ArrayList<>();

        @Override
        public Void visitCompilationUnit(CompilationUnitTree cu, Void nil) {
            pkg = cu.getPackageName() == null ? "" : cu.getPackageName().toString();
            return super.visitCompilationUnit(cu, nil);
        }

        @Override
        public Void visitClass(ClassTree ct, Void nil) {
            String name = ct.getSimpleName().toString();
            if (name.isEmpty() || name.equals("<error>")) return super.visitClass(ct, nil);
            String outer = cls;
            cls = cls.isEmpty() ? name : cls + "." + name;
            String fqn = pkg.isEmpty() ? cls : pkg + "." + cls;
            structs.add(fqn);
            flats.add((pkg.isEmpty() ? "" : pkg + ".") + cls.replace('.', '$'));
            Void r = super.visitClass(ct, nil);
            cls = outer;
            return r;
        }

        @Override
        public Void visitMethod(MethodTree mt, Void nil) {
            if (insideAnonymousClass()) return super.visitMethod(mt, nil);
            String name = mt.getName().toString();
            if (name.equals("<error>")) return super.visitMethod(mt, nil);
            List<String> params = paramStrings(symOfDecl(mt));
            String parentFqn = pkg.isEmpty() ? cls : pkg + "." + cls;
            funcs.add(parentFqn + "." + name + "(" + String.join(",", params) + ")");
            return super.visitMethod(mt, nil);
        }

        boolean insideAnonymousClass() {
            for (TreePath p = getCurrentPath().getParentPath(); p != null; p = p.getParentPath()) {
                if (p.getLeaf() instanceof ClassTree ct) {
                    return ct.getSimpleName().length() == 0;
                }
            }
            return false;
        }
    }

    /** Simple class name of an FQN (its last `.`/`$` segment). */
    static String simpleName(String fqn) {
        if (fqn == null) return "";
        int d = Math.max(fqn.lastIndexOf('.'), fqn.lastIndexOf('$'));
        return d >= 0 ? fqn.substring(d + 1) : fqn;
    }

    /** Simple class name -> canonical FQN, only for unambiguous names. */
    static Map<String, String> simpleStructIndex(Collection<String> surfaceStructs) {
        Map<String, List<String>> multi = new HashMap<>();
        for (String fqn : surfaceStructs) {
            String s = simpleName(fqn);
            if (s.isEmpty()) continue;
            multi.computeIfAbsent(s, k -> new ArrayList<>()).add(fqn);
        }
        return unambiguous(multi);
    }

    /**
     * Simple class name -> canonical `<init>` FQN for a constructor declared on
     * that class in the surface (only when unambiguous). Used to resolve a
     * `new X()` whose attribution degraded because X's bytecode was dropped.
     */
    static Map<String, String> constructorIndex(Map<String, String> surfaceFuncFqn) {
        Map<String, List<String>> multi = new HashMap<>();
        for (var e : surfaceFuncFqn.entrySet()) {
            String key = e.getKey();
            int p = key.indexOf('(');
            if (p < 0) continue;
            String decl = key.substring(0, p);
            int d = decl.lastIndexOf('.');
            if (d < 0 || !decl.substring(d + 1).equals("<init>")) continue;
            String s = simpleName(decl.substring(0, d));
            if (s.isEmpty()) continue;
            multi.computeIfAbsent(s, k -> new ArrayList<>()).add(e.getValue());
        }
        return unambiguous(multi);
    }

    /** Keeps only keys with a single distinct value (ambiguity is dropped). */
    static Map<String, String> unambiguous(Map<String, List<String>> multi) {
        Map<String, String> out = new HashMap<>();
        for (var e : multi.entrySet()) {
            Set<String> vals = new LinkedHashSet<>(e.getValue());
            if (vals.size() == 1) out.put(e.getKey(), vals.iterator().next());
        }
        return out;
    }

    static class Collector extends TreePathScanner<Void, Void> {
        String pkg = "", cls = "", mtd = "";
        String currentFile = "", sourceText = "";
        boolean emitting = false;
        final BufferedWriter out = new BufferedWriter(
            new OutputStreamWriter(System.out, StandardCharsets.UTF_8));

        // Opaque ids (SPEC §3): a single monotonic counter across structs and
        // methods. structID/funcID map canonical keys to ids; treeID records
        // which tree won a key, so a deduped (anonymous/local-collision)
        // declaration is skipped at emit time rather than causing a residual
        // FQN collision in the ingestor.
        int nextId = 0;
        final Map<String, String> structID = new HashMap<>();
        final Map<String, String> funcID = new HashMap<>();
        final IdentityHashMap<Tree, String> treeID = new IdentityHashMap<>();

        final Set<String> unresolvedSeen = new HashSet<>();
        final Set<String> emittedPkg = new HashSet<>();
        final String idPrefix;

        // Targeted-scan mode (phase-02 task-11): when true, a call/use whose
        // target is a project symbol NOT in the emitted target packages is
        // emitted against the target's canonical FQN — the unchanged file's
        // cached unit supplies the node, so the edge is exact. surfaceStructs /
        // surfaceFuncFqn are those unchanged packages' declaration surface.
        final boolean filtered;
        final Set<String> surfaceStructs;
        final Map<String, String> surfaceFuncFqn;
        // Phase-04 task-15: indexes over the WHOLE walked tree's declaration
        // surface so a degraded (error-symbol) attribution never leaks a bare
        // project-class simple name as an unresolved target —
        // `surfaceSimpleStruct` resolves an unambiguous simple name to its
        // canonical FQN.
        final Map<String, String> surfaceSimpleStruct;
        final Map<String, String> surfaceCtorBySimple;

        Collector(String idPrefix) {
            this(idPrefix, false, Set.of(), Map.of());
        }

        Collector(String idPrefix, boolean filtered,
                Set<String> surfaceStructs, Map<String, String> surfaceFuncFqn) {
            this.idPrefix = idPrefix;
            this.filtered = filtered;
            this.surfaceStructs = surfaceStructs;
            this.surfaceFuncFqn = surfaceFuncFqn;
            this.surfaceSimpleStruct = simpleStructIndex(surfaceStructs);
            this.surfaceCtorBySimple = constructorIndex(surfaceFuncFqn);
        }

        // Phase-04 task-35: the COMPLETE project-class index the resolveCall
        // fall-through consults on the filtered path. The surface indexes cover
        // the whole walked tree (task-15); the Collector's own target
        // declarations live in structID (FQN -> id) and funcID (function key ->
        // id). Both halves are unioned BY SIMPLE NAME — structID is keyed by
        // FQN, so a raw simple name can never match it directly. Built lazily:
        // structID/funcID are only complete after collectAll.
        private Map<String, String> projectStructBySimpleCache;
        private Map<String, String> projectCtorBySimpleCache;

        /** Simple class name -> canonical FQN over surface UNION target declarations. */
        Map<String, String> projectStructBySimple() {
            if (projectStructBySimpleCache == null) {
                Map<String, String> m = new HashMap<>(surfaceSimpleStruct);
                m.putAll(simpleStructIndex(structID.keySet()));
                projectStructBySimpleCache = m;
            }
            return projectStructBySimpleCache;
        }

        /** Simple class name -> canonical `<init>` FQN over surface UNION target declarations. */
        Map<String, String> projectCtorBySimple() {
            if (projectCtorBySimpleCache == null) {
                Map<String, String> m = new HashMap<>(surfaceCtorBySimple);
                m.putAll(constructorIndex(renderedFuncFqn(funcID)));
                projectCtorBySimpleCache = m;
            }
            return projectCtorBySimpleCache;
        }

        /**
         * The ingestor's function-FQN rendering for a key -> id map's keys:
         * a singleton (parent, name) group renders `parent.name`, an overloaded
         * group `parent.name(params)` — identical to runIncrementalScan's
         * surface rendering, so a resolved call into a target declaration
         * matches the full scan's FQN exactly.
         */
        static Map<String, String> renderedFuncFqn(Map<String, String> funcKeys) {
            Map<String, String> out = new HashMap<>();
            Map<String, List<String>> groups = new LinkedHashMap<>();
            for (String k : funcKeys.keySet()) {
                groups.computeIfAbsent(groupKey(k), x -> new ArrayList<>()).add(k);
            }
            for (List<String> keys : groups.values()) {
                for (String k : keys) {
                    out.put(k, keys.size() == 1 ? k.substring(0, k.indexOf('(')) : k);
                }
            }
            return out;
        }

        String newNodeID() {
            return idPrefix + (++nextId);
        }

        void emit(String line) {
            try {
                out.write(line);
                out.newLine();
            } catch (IOException e) {
                throw new RuntimeException(e);
            }
        }

        void emitEdge(String type, String from, String to) {
            emit("{\"type\":\"" + type + "\",\"from\":\"" + jstr(from)
                + "\",\"to\":\"" + jstr(to) + "\"}");
        }

        void emitUnresolvedCall(String from, String to) {
            emit("{\"type\":\"unresolved_call\",\"from\":\"" + jstr(from)
                + "\",\"to\":\"" + jstr(to) + "\"}");
        }

        /** Emits the unresolved node record for fqn on first encounter. */
        void unresolved(String fqn, String category) {
            if (fqn.isEmpty() || unresolvedSeen.contains(fqn)) return;
            unresolvedSeen.add(fqn);
            emit("{\"type\":\"unresolved\",\"fqn\":\"" + jstr(fqn)
                + "\",\"category\":\"" + jstr(category) + "\"}");
        }

        void emitPkgHierarchy(String pkg) {
            if (pkg.isEmpty()) return;
            String prev = "";
            String cur = "";
            for (String seg : pkg.split("\\.")) {
                cur = cur.isEmpty() ? seg : cur + "." + seg;
                if (!emittedPkg.contains(cur)) {
                    emittedPkg.add(cur);
                    emit("{\"type\":\"module\",\"fqn\":\"" + jstr(cur) + "\"}");
                }
                if (!prev.isEmpty()) {
                    emitEdge("contains", prev, cur);
                }
                prev = cur;
            }
        }

        /**
         * Emits the GLOBAL Module->Module scaffolding for packages outside the
         * per-file emission filter (feedback-103).
         *
         * The targeted scan re-emits only the target files' facts, but it walks
         * every source file (to build the class cache and package map). The
         * module hierarchy is not per-file emission: a full scan emits a
         * `module` record and `Module->Module` contains edge for every package it
         * walks, and the win-B assembled graph/export must match a full rebuild.
         * The target packages are covered by their own re-emitted files, so this
         * is called with the UNCHANGED packages only — the all-targets path
         * passes an empty set and stays byte-identical to the full scan.
         */
        void emitGlobalPkgScaffolding(Collection<String> packages) {
            for (String p : packages) emitPkgHierarchy(p);
        }

        void collectAll(List<CompilationUnitTree> units) {
            emitting = false;
            int i = 0;
            int total = units.size();
            for (var u : units) {
                scan(u, null);
                i++;
                progress("Collecting", i, total, fileBase(u));
            }
            endProgress();
        }

        void emitAll(List<CompilationUnitTree> units, int total) {
            emitting = true;
            int i = 0;
            for (var u : units) {
                scan(u, null);
                i++;
                progress("Emit", i, total, fileBase(u));
            }
            endProgress();
        }

        void flush() {
            try {
                out.flush();
                System.err.println();
            } catch (IOException e) {
                throw new RuntimeException(e);
            }
        }

        @Override
        public Void visitCompilationUnit(CompilationUnitTree cu, Void nil) {
            pkg = cu.getPackageName() == null ? "" : cu.getPackageName().toString();
            currentFile = cu.getSourceFile().toUri().getPath();
            try {
                sourceText = cu.getSourceFile().getCharContent(true).toString();
            } catch (Exception e) {
                sourceText = "";
            }
            if (emitting) {
                emitPkgHierarchy(pkg);
                LineMap lm = cu.getLineMap();
                long last = sourceText.isEmpty() ? 1 : lm.getLineNumber(sourceText.length() - 1);
                emit("{\"type\":\"file\",\"path\":\"" + jstr(currentFile)
                    + "\",\"parent\":\"" + jstr(pkg)
                    + "\",\"start_line\":1,\"end_line\":" + Math.max(1, last) + "}");
            }
            return super.visitCompilationUnit(cu, nil);
        }

        /**
         * Emits the `file` record for a WALKED source that is not a compilation
         * unit — a `module-info.java` descriptor. It stays in the scanned file
         * set (domain.entity.source-file: filtering is by code_type, never by
         * dropping the file) but is partitioned out of every javac batch
         * (partitionDescriptors), so no compilation unit ever emits it. The
         * record is byte-identical to visitCompilationUnit's for a unit: same
         * path rendering, `parent` "" (a descriptor declares no package), the
         * same `end_line` line count. Never a declaration/edge — a descriptor
         * has none.
         */
        void emitWalkedFile(Path f) {
            Path abs = f.toAbsolutePath().normalize();
            emit("{\"type\":\"file\",\"path\":\"" + jstr(abs.toString())
                + "\",\"parent\":\"\",\"start_line\":1,\"end_line\":" + Math.max(1, lineCountOf(abs)) + "}");
        }

        /**
         * The `end_line` a compilation unit in this file would report:
         * javac's 1-based line of the file's last character — the count of
         * lines carrying content (`"a\nb\n"` -> 2, `"a\nb"` -> 2, `"\n\n"` -> 2),
         * and 1 for an empty file, matching `visitCompilationUnit`'s
         * `Math.max(1, LineMap.getLineNumber(len-1))`.
         */
        static long lineCountOf(Path f) {
            try {
                String s = Files.readString(f, StandardCharsets.UTF_8);
                if (s.isEmpty()) return 1;
                long nl = s.chars().filter(c -> c == '\n').count();
                return s.charAt(s.length() - 1) == '\n' ? nl : nl + 1;
            } catch (IOException e) {
                return 1;
            }
        }

        @Override
        public Void visitClass(ClassTree ct, Void nil) {
            String name = ct.getSimpleName().toString();
            // Anonymous classes: scan their members but attribute them to the
            // enclosing named class (matches javac's $-name collapse).
            if (name.isEmpty()) return super.visitClass(ct, nil);
            // javac error recovery can synthesize a `<error>` class name; never
            // declare such a tree (same rationale as visitMethod).
            if (name.equals("<error>")) return super.visitClass(ct, nil);
            String outer = cls;
            cls = cls.isEmpty() ? name : cls + "." + name;
            String fqn = pkg.isEmpty() ? cls : pkg + "." + cls;

            if (!emitting) {
                if (!structID.containsKey(fqn)) {
                    String id = newNodeID();
                    structID.put(fqn, id);
                    treeID.put(ct, id);
                }
            } else {
                String id = treeID.get(ct);
                if (id != null) {
                    String parentFqn = outer.isEmpty() ? pkg : pkg.isEmpty() ? outer : pkg + "." + outer;
                    int[] span = classSpan((JCTree) ct);
                    emit("{\"type\":\"struct\",\"id\":\"" + jstr(id)
                        + "\",\"parent\":\"" + jstr(parentFqn)
                        + "\",\"name\":\"" + jstr(name)
                        + "\",\"path\":\"" + jstr(currentFile)
                        + "\",\"start\":" + span[0] + ",\"end\":" + span[1]
                        + ",\"start_line\":" + lineOf(span[0])
                        + ",\"end_line\":" + lineOf(span[1] - 1) + "}");
                    // Nested classes stay directly under their outer class;
                    // top-level classes are reached through the file node
                    // instead of the package (SPEC §7).
                    if (structID.containsKey(parentFqn)) {
                        emitEdge("contains", structID.get(parentFqn), id);
                    }
                    if (ct.getExtendsClause() != null) {
                        recordUse(id, ct.getExtendsClause());
                    }
                    for (var iface : ct.getImplementsClause()) {
                        recordUse(id, iface);
                    }
                }
            }
            Void r = super.visitClass(ct, nil);
            cls = outer;
            return r;
        }

        /** True if this method is declared inside an anonymous class. */
        boolean insideAnonymousClass() {
            for (TreePath p = getCurrentPath().getParentPath(); p != null; p = p.getParentPath()) {
                if (p.getLeaf() instanceof ClassTree ct) {
                    return ct.getSimpleName().length() == 0;
                }
            }
            return false;
        }

        @Override
        public Void visitMethod(MethodTree mt, Void nil) {
            // Methods of anonymous classes have no stable identity (javac even
            // synthesizes a body-less <init> for them); walk their bodies for
            // edges but never declare or register them.
            if (insideAnonymousClass()) {
                return super.visitMethod(mt, nil);
            }
            String name = mt.getName().toString();
            // javac error recovery: a method whose name is a keyword (e.g.
            // `void enum(...)`) is parsed with the synthetic name `<error>`.
            // It is not a real symbol; declaring it would pollute the graph and
            // can collide (two `<error>` declarations, or one with a sibling
            // `<error>` class tree). Skip the declaration but keep walking.
            if (name.equals("<error>")) {
                return super.visitMethod(mt, nil);
            }
            List<String> params = paramStrings(symOfDecl(mt));
            String parentFqn = pkg.isEmpty() ? cls : pkg + "." + cls;
            String key = parentFqn + "." + name + "(" + String.join(",", params) + ")";

            if (!emitting) {
                if (!funcID.containsKey(key)) {
                    String id = newNodeID();
                    funcID.put(key, id);
                    treeID.put(mt, id);
                }
            } else {
                String myId = treeID.get(mt);
                if (myId != null) {
                    int[] span = methodSpan((JCTree) mt);
                    StringBuilder paramsJson = new StringBuilder();
                    for (String p : params) {
                        if (paramsJson.length() > 0) paramsJson.append(',');
                        paramsJson.append('"').append(jstr(p)).append('"');
                    }
                    emit("{\"type\":\"function\",\"id\":\"" + jstr(myId)
                        + "\",\"parent\":\"" + jstr(parentFqn)
                        + "\",\"name\":\"" + jstr(name)
                        + "\",\"params\":[" + paramsJson + "]"
                        + ",\"file\":\"" + jstr(currentFile)
                        + "\",\"path\":\"" + jstr(currentFile)
                        + "\",\"start\":" + span[0] + ",\"end\":" + span[1]
                        + ",\"start_line\":" + lineOf(span[0])
                        + ",\"end_line\":" + lineOf(span[1] - 1) + "}");
                    if (structID.containsKey(parentFqn)) {
                        emitEdge("contains", structID.get(parentFqn), myId);
                    }
                }
                String prevMtd = mtd;
                mtd = funcID.get(key);
                Void r = super.visitMethod(mt, nil);
                mtd = prevMtd;
                return r;
            }
            return super.visitMethod(mt, nil);
        }

        @Override
        public Void visitMethodInvocation(MethodInvocationTree mc, Void nil) {
            if (!mtd.isEmpty()) {
                Symbol sym = symOf(mc);
                Type recv = receiverType(mc.getMethodSelect());
                resolveCall(sym, recv == null ? null : typeFqnOf(recv), methodRawName(mc.getMethodSelect()));
            }
            return super.visitMethodInvocation(mc, nil);
        }

        @Override
        public Void visitMemberReference(MemberReferenceTree mref, Void nil) {
            if (!mtd.isEmpty()) {
                Symbol sym = symOf(mref);
                resolveCall(sym, null, mref.getName().toString());
            }
            return super.visitMemberReference(mref, nil);
        }

        @Override
        public Void visitNewClass(NewClassTree nc, Void nil) {
            if (!mtd.isEmpty()) {
                recordUse(mtd, nc.getIdentifier());
                Symbol sym = symOf(nc);
                resolveCall(sym, typeFqn(nc.getIdentifier()), typeRawName(nc.getIdentifier()));
            }
            return super.visitNewClass(nc, nil);
        }

        @Override
        public Void visitVariable(VariableTree vt, Void nil) {
            if (!mtd.isEmpty() && vt.getType() != null) recordUse(mtd, vt.getType());
            return super.visitVariable(vt, nil);
        }

        @Override
        public Void visitInstanceOf(InstanceOfTree io, Void nil) {
            if (!mtd.isEmpty() && io.getType() != null) recordUse(mtd, io.getType());
            return super.visitInstanceOf(io, nil);
        }

        @Override
        public Void visitTypeCast(TypeCastTree tc, Void nil) {
            if (!mtd.isEmpty() && tc.getType() != null) recordUse(mtd, tc.getType());
            return super.visitTypeCast(tc, nil);
        }

        /**
         * Resolves a method call to an edge. Exact in-project methods become
         * `calls` (matched by erased parameter types, so overloads and
         * constructors disambiguate). Otherwise the receiver type (if a project
         * struct) becomes `uses`; everything else an `unresolved_call`.
         */
        void resolveCall(Symbol sym, String recvFqn, String rawName) {
            if (sym instanceof Symbol.MethodSymbol ms) {
                // Calls into an anonymous class's own synthetic members (e.g.
                // its generated <init> calling super()) have no stable identity.
                Symbol directOwner = ms.getEnclosingElement();
                if (directOwner instanceof Symbol.ClassSymbol ocs && ocs.getQualifiedName().length() == 0) {
                    return;
                }
                String parent = ownerFqn(directOwner);
                String name = ms.getSimpleName().toString();
                // In targeted mode an error-symbol parent is not a resolvable
                // project method (phase-04 task-15); the full scan keeps its
                // exact behaviour, so this guard is filtered-only.
                if (parent != null && (!filtered || !parent.contains("<error>"))
                        && (name.equals("<init>") || name.indexOf('<') < 0) && !name.equals("<error>")) {
                    String key = parent + "." + name + "(" + String.join(",", paramStrings(ms)) + ")";
                    String id = funcID.get(key);
                    if (id != null) {
                        emitEdge("calls", mtd, id);
                        return;
                    }
                    // Targeted scan: the method belongs to an unchanged
                    // (non-emitted) project package — emit against its
                    // canonical FQN so the cached node resolves the edge.
                    String canon = filtered ? surfaceFuncFqn.get(key) : null;
                    if (canon != null) {
                        emitEdge("calls", mtd, canon);
                        return;
                    }
                    String fqn = name.equals("<init>") ? parent + ".<init>" : parent + "." + name;
                    unresolved(fqn, categoryOf(parent));
                    emitUnresolvedCall(mtd, fqn);
                    return;
                }
            }
            // Phase-04 task-35: a degraded/error-symbol call must never leak a
            // bare project-class simple name. The COMPLETE project-class index
            // (the task-15 surface over the whole walked tree UNION the target
            // declarations this Collector holds in structID/funcID) is consulted
            // BY SIMPLE NAME: a constructor resolves to its canonical <init>
            // FQN, and a project class with no declared constructor in the index
            // resolves to the same `<FQN>.<init>` unresolved target the FULL
            // context emits for its (implicit) constructor. A name the index
            // cannot resolve to a single project class falls through to EXACTLY
            // the full scan's own emission: the full scan is the exactness
            // oracle, so suppressing a record it emits would be a deficit (the
            // note-87 divergence direction). Only a genuine error symbol is
            // withheld. The full scan (unfiltered) keeps its byte-identical
            // behaviour.
            if (filtered && rawName != null && !rawName.contains("<error>")) {
                String ctor = projectCtorBySimple().get(rawName);
                if (ctor != null) {
                    emitEdge("calls", mtd, ctor);
                    return;
                }
                String structFqn = projectStructBySimple().get(rawName);
                if (structFqn != null) {
                    String fqn = structFqn + ".<init>";
                    unresolved(fqn, categoryOf(structFqn));
                    emitUnresolvedCall(mtd, fqn);
                    return;
                }
            }
            if (recvFqn != null && structID.containsKey(recvFqn)) {
                emitEdge("uses", mtd, structID.get(recvFqn));
            } else if (recvFqn != null && filtered && surfaceStructs.contains(recvFqn)) {
                emitEdge("uses", mtd, recvFqn);
            } else {
                String target = rawName == null ? "?" : rawName;
                if (filtered && target.contains("<error>")) return;
                unresolved(target, "unknown");
                emitUnresolvedCall(mtd, target);
            }
        }

        /**
         * Records a type use: a project struct becomes a resolved `uses` edge,
         * an unresolvable type an `unresolved_use`. Resolved external types
         * are dropped (ubiquitous, low-signal).
         */
        void recordUse(String fromId, Tree type) {
            String tfqn = typeFqn(type);
            if (tfqn != null) {
                if (structID.containsKey(tfqn)) {
                    emitEdge("uses", fromId, structID.get(tfqn));
                } else if (filtered && surfaceStructs.contains(tfqn)) {
                    emitEdge("uses", fromId, tfqn);
                }
                return;
            }
            String raw = typeRawName(type);
            if (raw == null) raw = "?";
            // Phase-04 task-36: the previous pass's `structID.containsKey(raw)`
            // compared a RAW SIMPLE NAME against structID's FQN keys — it could
            // never match, and the surrounding surface reconstruction was built
            // on it. Both are GONE: `recordUse` is a pure function of the tree's
            // own attributed type, so when the FULL context degraded the tree
            // (tfqn null) the full scan emits this same fall-through, and
            // withholding or "resolving" it here would be a divergence from the
            // exactness oracle — measured: reconstructing the unambiguous nested
            // simple names the full context itself cannot resolve yields a
            // deficit (DijkstraSearchFrontier / ContractionSearchFrontier /
            // DijkstraClosestFirstIterator at jgrapht-core scale). The complete
            // project-class index is consulted by resolveCall (task-35), where it
            // reconstructs the correct resolved call; a type-use reference that
            // javac could not resolve in the full context is emitted exactly as
            // the full scan emits it. Only a genuine error symbol is withheld.
            if (filtered && raw.contains("<error>")) return;
            unresolved(raw, "unknown");
            emitEdge("unresolved_use", fromId, raw);
        }

        /** Static type of the receiver expression of a method select, if any. */
        static Type receiverType(ExpressionTree sel) {
            if (!(sel instanceof MemberSelectTree ms)) return null;
            ExpressionTree expr = ms.getExpression();
            if (!(expr instanceof JCTree jc)) return null;
            return jc.type;
        }

        static String typeFqnOf(Type t) {
            if (t == null) return null;
            if (t.tsym instanceof Symbol.ClassSymbol cs) {
                String qn = cs.getQualifiedName().toString();
                if (qn.indexOf('<') >= 0 || qn.contains("<error>")) return null;
                return qn;
            }
            return null;
        }

        /** 1-based line number of a character position, clamped to 1. */
        int lineOf(int pos) {
            if (pos < 0) return 1;
            long l = getCurrentPath().getCompilationUnit().getLineMap().getLineNumber(pos);
            return l > 0 ? (int) l : 1;
        }

        /** 0-based span of a class declaration, including its doc comment. */
        int[] classSpan(JCTree jc) {
            JCTree.JCCompilationUnit jcCu = (JCTree.JCCompilationUnit) getCurrentPath().getCompilationUnit();
            int start = jc.getStartPosition();
            if (jcCu.docComments != null && jcCu.docComments.hasComment(jc)) {
                for (int p = start; p >= 2; p--) {
                    if (sourceText.charAt(p - 1) == '*' && sourceText.charAt(p - 2) == '/') {
                        start = p - 2;
                        break;
                    }
                }
            }
            int bracePos = sourceText.indexOf('{', jc.getStartPosition());
            int end = jc.getEndPosition(jcCu.endPositions);
            if (end <= start) {
                if (bracePos > 0) {
                    int close = matchingBraceEnd(sourceText, bracePos);
                    end = close > 0 ? close + 1 : bracePos + 1;
                } else {
                    end = start;
                }
            }
            return new int[]{start, end};
        }

        /** 0-based span of a method declaration, including its doc comment. */
        int[] methodSpan(JCTree jc) {
            JCTree.JCCompilationUnit jcCu = (JCTree.JCCompilationUnit) getCurrentPath().getCompilationUnit();
            int start = jc.getStartPosition();
            if (jcCu.docComments != null && jcCu.docComments.hasComment(jc)) {
                for (int p = start; p >= 2; p--) {
                    if (sourceText.charAt(p - 1) == '*' && sourceText.charAt(p - 2) == '/') {
                        start = p - 2;
                        break;
                    }
                }
            }
            int end = jc.getEndPosition(jcCu.endPositions);
            if (end < 0 && jc instanceof JCTree.JCMethodDecl md && md.body != null) {
                end = md.body.getEndPosition(jcCu.endPositions);
            }
            if (end < 0) {
                int open = sourceText.indexOf('{', start);
                if (open >= 0) {
                    int close = matchingBraceEnd(sourceText, open);
                    if (close >= 0) end = close + 1;
                }
            }
            if (end < start) end = start;
            return new int[]{start, end};
        }

        /**
         * Returns the index just past the closing brace that matches the
         * opening brace at {@code open}, skipping strings, chars, and comments.
         */
        static int matchingBraceEnd(String s, int open) {
            int depth = 0;
            for (int i = open; i < s.length(); i++) {
                char c = s.charAt(i);
                if (c == '"') {
                    for (i++; i < s.length() && s.charAt(i) != '"'; i++) {
                        if (s.charAt(i) == '\\') i++;
                    }
                } else if (c == '\'') {
                    for (i++; i < s.length() && s.charAt(i) != '\''; i++) {
                        if (s.charAt(i) == '\\') i++;
                    }
                } else if (c == '/' && i + 1 < s.length()) {
                    if (s.charAt(i + 1) == '/') {
                        while (i < s.length() && s.charAt(i) != '\n') i++;
                    } else if (s.charAt(i + 1) == '*') {
                        i += 2;
                        while (i + 1 < s.length() && !(s.charAt(i) == '*' && s.charAt(i + 1) == '/')) i++;
                        i++;
                    }
                } else if (c == '{') {
                    depth++;
                } else if (c == '}') {
                    depth--;
                    if (depth == 0) return i;
                }
            }
            return -1;
        }
    }
}
