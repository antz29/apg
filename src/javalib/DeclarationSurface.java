import com.sun.source.tree.*;
import com.sun.source.util.*;
import javax.tools.*;
import java.nio.file.*;
import java.util.*;

class DeclarationSurface {
    /**
     * Compiles a batch of sources into the class dir (bytecode generation must
     * happen even when an unrelated file has errors, so the error-stop policy
     * is OFF here), and records the declaration surface of the files in
     * `collect`. A batch that crashes javac is split (binary isolation) so one
     * bad file never discards the rest; a single crashing file is dropped,
     * matching the full scan's behaviour.
     */
    static void compileAndCollect(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, List<String> opts, Map<Path, ClassSurfaceCache.FileRec> out,
            Set<Path> collect) {
        if (files.isEmpty()) return;
        try {
            var task = JavacSupport.newTask(compiler, fm, files, opts, false);
            List<CompilationUnitTree> units = new ArrayList<>();
            for (var u : task.parse()) units.add(u);
            for (var u : task.analyze()) {
                /* drive attribution */
            }
            for (var u : units) {
                Path abs = Paths.get(u.getSourceFile().toUri()).toAbsolutePath().normalize();
                if (!collect.contains(abs)) continue;
                var sc = new SurfaceScanner();
                sc.useTask(task);
                sc.scan(u, null);
                ClassSurfaceCache.FileRec rec = new ClassSurfaceCache.FileRec();
                rec.compiled = true;
                rec.hash = ClassSurfaceCache.sha1(abs);
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
                System.err.println("  [" + Progress.elapsed() + "] dropping un-attributable file: " + files.get(0));
                // Phase-04 task-31: a dropped source must still contribute its
                // declaration surface. If the crash happened after collection
                // (e.g. during generate()) the record is already present and is
                // kept; otherwise recover it from a parse-only pass — the file
                // must never silently vanish from a COMPLETE surface.
                if (collect.contains(abs) && !out.containsKey(abs)) {
                    ClassSurfaceCache.FileRec rec = surfaceFromSource(files.get(0));
                    if (rec != null) {
                        out.put(abs, rec);
                    } else {
                        System.err.println("  [" + Progress.elapsed() + "] no declaration surface for dropped file: "
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
    static ClassSurfaceCache.FileRec surfaceFromSource(Path file) {
        try {
            var compiler = ToolProvider.getSystemJavaCompiler();
            var fm = compiler.getStandardFileManager(null, null, null);
            var task = JavacSupport.newTask(compiler, fm, List.of(file));
            for (CompilationUnitTree u : task.parse()) {
                var sc = new SurfaceScanner();
                sc.useTask(task);
                sc.scan(u, null);
                ClassSurfaceCache.FileRec rec = new ClassSurfaceCache.FileRec();
                rec.compiled = true;
                rec.hash = ClassSurfaceCache.sha1(file.toAbsolutePath().normalize());
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
        Trees trees;

        /** Attach the public-API attribution services for this task. */
        void useTask(JavacTask task) {
            this.trees = Trees.instance(task);
        }

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
            List<String> params = SymbolNaming.paramStrings(SymbolNaming.symOfDecl(trees, getCurrentPath(), mt));
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

    /** Simple class name -> canonical FQN, only for unambiguous names. */
    static Map<String, String> simpleStructIndex(Collection<String> surfaceStructs) {
        Map<String, List<String>> multi = new HashMap<>();
        for (String fqn : surfaceStructs) {
            String s = SymbolNaming.simpleName(fqn);
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
            String s = SymbolNaming.simpleName(decl.substring(0, d));
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
}
