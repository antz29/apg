import com.sun.source.tree.CompilationUnitTree;
import javax.tools.*;
import java.nio.file.*;
import java.util.*;

class ScanRunner {
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
        SourceDiscovery.partitionDescriptors(files, sources, descriptors);
        // Snapshot the attributable sources before crashing-file isolation
        // mutates `sources`: the seeded surface must be complete over every
        // attributable input (a descriptor declares nothing to surface).
        List<Path> allFiles = new ArrayList<>(sources);
        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        var fm = compiler.getStandardFileManager(null, null, null);

        // Parse everything first (declarations are collected/emitted from the
        // parse tree; edges are resolved against attributed symbols).
        var task = JavacSupport.newTask(compiler, fm, sources);
        int total = sources.size();
        var units = new ArrayList<CompilationUnitTree>();
        int i = 0;
        for (CompilationUnitTree unit : task.parse()) {
            units.add(unit);
            i++;
            Progress.progress("Parsing", i, total, SourceDiscovery.fileBase(unit));
        }
        Progress.endProgress();

        // Attribute for exact call/type resolution. javac can crash on a few
        // files (e.g. switch-expression AssertionError on JDK 17); isolate and
        // drop those files, then re-attribute the rest so the graph keeps
        // exact edges everywhere else.
        List<Path> crashing = new ArrayList<>();
        if (!JavacSupport.tryAnalyze(task, total)) {
            System.err.println("WARNING: attribution crashed; isolating offending files...");
            crashing = JavacSupport.findCrashingFiles(compiler, fm, sources);
            System.err.println();
            System.err.println("WARNING: excluding " + crashing.size() + " files from attribution: " + crashing);
            sources.removeAll(crashing);
            System.err.println("[" + Progress.elapsed() + "] re-parsing and re-attributing " + sources.size() + " remaining files...");
            task = JavacSupport.newTask(compiler, fm, sources);
            units.clear();
            for (var unit : task.parse()) units.add(unit);
            JavacSupport.tryAnalyze(task, total);
        }

        System.err.println("[" + Progress.elapsed() + "] pass 1: assigning ids to declared classes and methods...");
        var c = new Collector(prefix);
        c.useTask(task);
        // Pass 1: assign opaque ids to every declared class and method.
        c.collectAll(units);
        System.err.println("[" + Progress.elapsed() + "] pass 2: emitting nodes and edges...");
        // Pass 2: emit node + edge records, resolving endpoints by id.
        c.emitAll(units, total);
        // The walked module descriptors: a `file` record each, byte-identical to
        // a compilation unit's, emitted from the walked set exactly once (they
        // are in no attributed batch, so no unit ever emits them). feedback-138.
        for (Path d : descriptors) c.emitWalkedFile(d);
        c.writer.flush();

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
            System.err.println("[" + Progress.elapsed() + "] seeding java class cache for "
                + wanted.size() + " file(s) into " + javaRoot);
            Map<Path, ClassSurfaceCache.FileRec> raw = new LinkedHashMap<>();
            var compiler = ToolProvider.getSystemJavaCompiler();
            var fm = compiler.getStandardFileManager(null, null, null);
            DeclarationSurface.compileAndCollect(compiler, fm, new ArrayList<>(allFiles), classOpts, raw, wanted);
            // The surface the targeted path consumes is keyed relative to the
            // scan root (runIncrementalScan does `root.resolve(rel)`).
            Map<String, ClassSurfaceCache.FileRec> surface = new LinkedHashMap<>();
            for (var e : raw.entrySet()) surface.put(ClassSurfaceCache.relOf(root, e.getKey()), e.getValue());
            if (surface.size() != wanted.size()) {
                System.err.println("WARNING: java class surface incomplete ("
                    + surface.size() + "/" + wanted.size()
                    + " file(s) surfaced); not persisting a partial surface");
                return;
            }
            ClassSurfaceCache.saveSurface(javaRoot.resolve("surface.tsv"), surface);
            System.err.println("[" + Progress.elapsed() + "] persisted java class surface for "
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
        SourceDiscovery.partitionDescriptors(allFiles, sourceList, descriptorList);

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
        Map<Path, String> pkgByFile = JavacSupport.packageMap(ToolProvider.getSystemJavaCompiler(),
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
            // Every walked package — the default (empty) package included.
            // emitPkgHierarchy maps "" to the non-empty default-package
            // identity, so dropping it here would omit the `(default)` module
            // record a full scan emits (feedback-103).
            LinkedHashSet<String> allPkgs = new LinkedHashSet<>(pkgByFile.values());
            System.err.println("[" + Progress.elapsed() + "] targets matched no attributable scanned file; "
                + "emitting global module scaffolding for " + allPkgs.size() + " package(s)"
                + (requestedDescriptors.isEmpty() ? " only"
                    : " + " + requestedDescriptors.size() + " module descriptor file(s)"));
            var c = new Collector(prefix, true, Set.of(), Map.of());
            c.emitGlobalPkgScaffolding(allPkgs);
            for (Path d : requestedDescriptors) c.emitWalkedFile(d);
            c.writer.flush();
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
        System.err.println("[" + Progress.elapsed() + "] incremental: " + targetFiles.size()
            + " target file(s) in " + targetPkgs.size() + " package(s); "
            + nonTargetFiles.size() + " reused from the class cache");

        // Load the compiled-declaration surface persisted from earlier scans.
        Map<String, ClassSurfaceCache.FileRec> surface = persistent ? ClassSurfaceCache.loadSurface(surfaceFile) : new HashMap<>();

        // Drop records (and their bytecode) for files that are now targets or
        // gone; compile only the non-target files whose cache entry is missing
        // or stale (content hash changed).
        List<String> staleRels = new ArrayList<>();
        for (String rel : surface.keySet()) {
            if (!nonTargetFiles.contains(root.resolve(rel))) staleRels.add(rel);
        }
        for (String rel : staleRels) {
            ClassSurfaceCache.FileRec rec = surface.remove(rel);
            if (rec != null) ClassSurfaceCache.deleteClasses(classesDir, rec);
        }

        Map<String, ClassSurfaceCache.FileRec> clean = new LinkedHashMap<>();
        List<Path> dirty = new ArrayList<>();
        for (Path f : nonTargetFiles) {
            String rel = ClassSurfaceCache.relOf(root, f);
            ClassSurfaceCache.FileRec rec = surface.get(rel);
            String hash = ClassSurfaceCache.sha1(f);
            if (rec != null && rec.compiled && rec.hash.equals(hash) && ClassSurfaceCache.classesPresent(classesDir, rec)) {
                clean.put(rel, rec);
            } else {
                if (rec != null) ClassSurfaceCache.deleteClasses(classesDir, rec);
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
                System.err.println("[" + Progress.elapsed() + "] compiling " + dirty.size()
                    + " unchanged-package file(s) into " + classesDir);
            }
            LinkedHashSet<Path> batch = new LinkedHashSet<>(dirty);
            batch.addAll(targetFiles);
            Map<Path, ClassSurfaceCache.FileRec> fresh = new LinkedHashMap<>();
            var ccompiler = ToolProvider.getSystemJavaCompiler();
            var cfm = ccompiler.getStandardFileManager(null, null, null);
            DeclarationSurface.compileAndCollect(ccompiler, cfm, new ArrayList<>(batch), classOpts, fresh, batch);
            for (var e : fresh.entrySet()) clean.put(ClassSurfaceCache.relOf(root, e.getKey()), e.getValue());
            Progress.endProgress();
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
            String rel = ClassSurfaceCache.relOf(root, f);
            if (clean.containsKey(rel)) continue;
            ClassSurfaceCache.FileRec rec = DeclarationSurface.surfaceFromSource(f);
            if (rec != null) {
                System.err.println("  [" + Progress.elapsed() + "] surfaced un-attributable file: " + rel);
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
        for (ClassSurfaceCache.FileRec rec : clean.values()) {
            surfaceStructs.addAll(rec.structs);
            for (String k : rec.funcs) {
                funcGroups.computeIfAbsent(SymbolNaming.groupKey(k), x -> new ArrayList<>()).add(k);
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
                    Path linked = SourceDiscovery.moduleInfoFreeRoot(p, rootsBase.resolve("r" + rootsIdx++));
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
        var task = JavacSupport.newTask(tcompiler, tfm, tfiles, targetOpts);
        var units = new ArrayList<CompilationUnitTree>();
        for (CompilationUnitTree unit : task.parse()) units.add(unit);
        if (!JavacSupport.tryAnalyze(task, total)) {
            System.err.println("WARNING: attribution crashed; isolating offending files...");
            List<Path> crashing = JavacSupport.findCrashingFiles(tcompiler, tfm, tfiles, targetOpts);
            System.err.println();
            System.err.println("WARNING: excluding " + crashing.size() + " files from attribution: " + crashing);
            tfiles.removeAll(crashing);
            task = JavacSupport.newTask(tcompiler, tfm, tfiles, targetOpts);
            units.clear();
            for (var unit : task.parse()) units.add(unit);
            JavacSupport.tryAnalyze(task, tfiles.size());
        }

        System.err.println("[" + Progress.elapsed() + "] pass 1: assigning ids to declared classes and methods...");
        var c = new Collector(prefix, true, surfaceStructs, surfaceFuncFqn);
        c.useTask(task);
        c.collectAll(units);
        // feedback-103: emit the global Module->Module scaffolding for the
        // unchanged packages the per-file filter leaves out. Target packages
        // get theirs from their own re-emitted files, so the union covers every
        // walked package — matching a full scan's hierarchy. The all-targets
        // path has no unchanged package and stays byte-identical to a full scan.
        // The default (empty) package is kept: emitPkgHierarchy maps it to the
        // non-empty default-package identity, so an unchanged default package
        // still contributes its `(default)` module record to the global
        // scaffolding (feedback-103). A default-package TARGET never reaches
        // here (package granularity puts every default-package file in the
        // re-emitted set), so this cannot double-emit.
        LinkedHashSet<String> nonTargetPkgs = new LinkedHashSet<>();
        for (Path f : nonTargetFiles) nonTargetPkgs.add(pkgByFile.getOrDefault(f, ""));
        System.err.println("[" + Progress.elapsed() + "] emitting global module scaffolding for "
            + nonTargetPkgs.size() + " reused package(s)...");
        c.emitGlobalPkgScaffolding(nonTargetPkgs);
        System.err.println("[" + Progress.elapsed() + "] pass 2: emitting nodes and edges...");
        c.emitAll(units, total);
        // The module descriptors named by the re-emission target set: a `file`
        // record each, emitted from the walked set (they are attributed
        // nowhere), exactly once and byte-identical to the full leg's — so a
        // changed module-info.java keeps its File node without any descriptor
        // ever entering an attribution batch. A non-target descriptor is not
        // re-emitted here; its File node is reused from the cached fact unit,
        // exactly like any unchanged file. feedback-138.
        for (Path d : requestedDescriptors) c.emitWalkedFile(d);
        c.writer.flush();

        // Persist the surface for the next scan (atomic replace).
        if (persistent) {
            Map<String, ClassSurfaceCache.FileRec> out = new LinkedHashMap<>(clean);
            ClassSurfaceCache.saveSurface(surfaceFile, out);
        }
        SourceDiscovery.deleteRec(rootsBase);
    }
}
