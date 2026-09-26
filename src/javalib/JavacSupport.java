import com.sun.source.tree.CompilationUnitTree;
import com.sun.source.util.*;
import javax.tools.*;
import java.nio.file.*;
import java.util.*;

class JavacSupport {
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
            if (e.getCompilationUnit() != null) return SourceDiscovery.fileBase(e.getCompilationUnit());
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
                        Progress.progress("Attributing", n[0], total, eventFile(e));
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
            Progress.endProgress();
            return true;
        } catch (Throwable t) {
            Progress.endProgress();
            return false;
        }
    }

    /** Binary-search the files that crash javac attribution. */
    static List<Path> findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> files) {
        System.err.println("[" + Progress.elapsed() + "] binary-searching " + files.size()
            + " files; each probe re-parses+re-attributes a chunk (slow)...");
        List<Path> crashing = new ArrayList<>();
        findCrashingFiles(compiler, fm, files, 0, files.size(), crashing);
        return crashing;
    }

    static void findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, int lo, int hi, List<Path> out) {
        if (lo >= hi) return;
        if (hi - lo == 1) {
            System.err.println("  [" + Progress.elapsed() + "] crashing file: " + files.get(lo));
            out.add(files.get(lo));
            return;
        }
        int mid = (lo + hi) / 2;
        System.err.println("[" + Progress.elapsed() + "] probing chunk [" + lo + "," + mid + ") of "
            + files.size() + " (" + (mid - lo) + " files)...");
        if (chunkCrashes(compiler, fm, files.subList(lo, mid))) {
            findCrashingFiles(compiler, fm, files, lo, mid, out);
        }
        System.err.println("[" + Progress.elapsed() + "] probing chunk [" + mid + "," + hi + ") of "
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
        System.err.println("[" + Progress.elapsed() + "] binary-searching " + files.size()
            + " files; each probe re-parses+re-attributes a chunk (slow)...");
        List<Path> crashing = new ArrayList<>();
        findCrashingFiles(compiler, fm, files, 0, files.size(), crashing, opts);
        return crashing;
    }

    static void findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, int lo, int hi, List<Path> out, List<String> opts) {
        if (lo >= hi) return;
        if (hi - lo == 1) {
            System.err.println("  [" + Progress.elapsed() + "] crashing file: " + files.get(lo));
            out.add(files.get(lo));
            return;
        }
        int mid = (lo + hi) / 2;
        System.err.println("[" + Progress.elapsed() + "] probing chunk [" + lo + "," + mid + ") of "
            + files.size() + " (" + (mid - lo) + " files)...");
        if (chunkCrashes(compiler, fm, files.subList(lo, mid), opts)) {
            findCrashingFiles(compiler, fm, files, lo, mid, out, opts);
        }
        System.err.println("[" + Progress.elapsed() + "] probing chunk [" + mid + "," + hi + ") of "
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
}
