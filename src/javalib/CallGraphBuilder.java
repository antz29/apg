import com.sun.source.tree.*;
import com.sun.source.util.*;
import com.sun.tools.javac.code.Symbol;
import com.sun.tools.javac.code.Type;
import com.sun.tools.javac.tree.JCTree;
import javax.tools.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
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
        try (var walk = Files.walk(dir)) {
            walk.filter(p -> p.toString().endsWith(".java"))
                .filter(Files::isRegularFile)
                .filter(p -> excludePaths.stream().noneMatch(pat -> p.toString().contains(pat)))
                .forEach(files::add);
        }
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
            runFullScan(files, prefix);
        } else {
            runIncrementalScan(dir, files, targets, prefix, cacheDir, cacheKey);
        }
    }

    /** The full-context scan: parse everything, attribute everything, emit everything. */
    static void runFullScan(List<Path> files, String prefix) throws Exception {
        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        var fm = compiler.getStandardFileManager(null, null, null);

        // Parse everything first (declarations are collected/emitted from the
        // parse tree; edges are resolved against attributed symbols).
        var task = newTask(compiler, fm, files);
        int total = files.size();
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
            crashing = findCrashingFiles(compiler, fm, files);
            System.err.println();
            System.err.println("WARNING: excluding " + crashing.size() + " files from attribution: " + crashing);
            files.removeAll(crashing);
            System.err.println("[" + elapsed() + "] re-parsing and re-attributing " + files.size() + " remaining files...");
            task = newTask(compiler, fm, files);
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
        c.flush();
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

        // Caller-provided target paths are absolute; map them onto the walked tree.
        Map<Path, Path> walked = new LinkedHashMap<>();
        for (Path f : allFiles) walked.putIfAbsent(f.toAbsolutePath().normalize(), f);
        Set<Path> requested = new LinkedHashSet<>();
        for (Path t : targetList) {
            Path a = t.toAbsolutePath().normalize();
            if (walked.containsKey(a)) requested.add(a);
        }
        if (requested.isEmpty()) {
            // A non-empty target list that matches no walked file selects
            // nothing (an explicit filter is in force), never everything.
            System.err.println("[" + elapsed() + "] targets matched no scanned file; emitting nothing");
            return;
        }

        // Package of every file (the re-emission unit for Java). Parsing is
        // cheap; attribution is the expensive part we keep to the target set.
        // Phase 02 note: java compilers/file managers carry option state across
        // tasks in one JVM, so every task gets a fresh compiler + file manager.
        Map<Path, String> pkgByFile = packageMap(ToolProvider.getSystemJavaCompiler(),
                new ArrayList<>(walked.keySet()));
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
        // their dependents may still reference them) — and collect the
        // UNCHANGED files' declaration surface. Crashing files are isolated
        // and dropped, exactly like the full scan.
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
            compileAndCollect(ccompiler, cfm, new ArrayList<>(batch), classOpts, fresh, nonTargetFiles);
            for (var e : fresh.entrySet()) clean.put(relOf(root, e.getKey()), e.getValue());
            endProgress();
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
        List<Path> tfiles = new ArrayList<>(targetFiles);
        int total = tfiles.size();
        var tcompiler = ToolProvider.getSystemJavaCompiler();
        var tfm = tcompiler.getStandardFileManager(null, null, null);
        var task = newTask(tcompiler, tfm, tfiles, List.of("-classpath", classesDir.toString()));
        var units = new ArrayList<CompilationUnitTree>();
        for (CompilationUnitTree unit : task.parse()) units.add(unit);
        if (!tryAnalyze(task, total)) {
            System.err.println("WARNING: attribution crashed; isolating offending files...");
            List<Path> crashing = findCrashingFiles(tcompiler, tfm, tfiles,
                List.of("-classpath", classesDir.toString()));
            System.err.println();
            System.err.println("WARNING: excluding " + crashing.size() + " files from attribution: " + crashing);
            tfiles.removeAll(crashing);
            task = newTask(tcompiler, tfm, tfiles, List.of("-classpath", classesDir.toString()));
            units.clear();
            for (var unit : task.parse()) units.add(unit);
            tryAnalyze(task, tfiles.size());
        }

        System.err.println("[" + elapsed() + "] pass 1: assigning ids to declared classes and methods...");
        var c = new Collector(prefix, true, surfaceStructs, surfaceFuncFqn);
        c.collectAll(units);
        System.err.println("[" + elapsed() + "] pass 2: emitting nodes and edges...");
        c.emitAll(units, total);
        c.flush();

        // Persist the surface for the next scan (atomic replace).
        if (persistent) {
            Map<String, FileRec> out = new LinkedHashMap<>(clean);
            saveSurface(surfaceFile, out);
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
                System.err.println("  [" + elapsed() + "] dropping un-attributable file: " + files.get(0));
                return;
            }
            int mid = files.size() / 2;
            compileAndCollect(compiler, fm, new ArrayList<>(files.subList(0, mid)), opts, out, collect);
            compileAndCollect(compiler, fm, new ArrayList<>(files.subList(mid, files.size())), opts, out, collect);
        }
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

        Collector(String idPrefix) {
            this(idPrefix, false, Set.of(), Map.of());
        }

        Collector(String idPrefix, boolean filtered,
                Set<String> surfaceStructs, Map<String, String> surfaceFuncFqn) {
            this.idPrefix = idPrefix;
            this.filtered = filtered;
            this.surfaceStructs = surfaceStructs;
            this.surfaceFuncFqn = surfaceFuncFqn;
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
                if (parent != null && (name.equals("<init>") || name.indexOf('<') < 0) && !name.equals("<error>")) {
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
            if (recvFqn != null && structID.containsKey(recvFqn)) {
                emitEdge("uses", mtd, structID.get(recvFqn));
            } else if (recvFqn != null && filtered && surfaceStructs.contains(recvFqn)) {
                emitEdge("uses", mtd, recvFqn);
            } else {
                String target = rawName == null ? "?" : rawName;
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
            String raw = typeRawName(type);
            if (tfqn == null) {
                unresolved(raw == null ? "?" : raw, "unknown");
                emitEdge("unresolved_use", fromId, raw == null ? "?" : raw);
            } else if (structID.containsKey(tfqn)) {
                emitEdge("uses", fromId, structID.get(tfqn));
            } else if (filtered && surfaceStructs.contains(tfqn)) {
                emitEdge("uses", fromId, tfqn);
            }
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
