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
    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        List<String> excludePaths = new ArrayList<>();
        for (int i = 1; i < args.length; i++) excludePaths.add(args[i]);

        List<Path> files = new ArrayList<>();
        try (var walk = Files.walk(dir)) {
            walk.filter(p -> p.toString().endsWith(".java"))
                .filter(Files::isRegularFile)
                .filter(p -> excludePaths.stream().noneMatch(pat -> p.toString().contains(pat)))
                .forEach(files::add);
        }

        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        var fm = compiler.getStandardFileManager(null, null, null);

        // Parse everything first (declarations are emitted from the parse tree).
        var task = newTask(compiler, fm, files);
        int total = files.size();
        var units = new ArrayList<CompilationUnitTree>();
        int i = 0;
        for (var unit : task.parse()) {
            units.add(unit);
            i++;
            System.err.print("\rParsing: " + (i * 100 / total) + "% (" + i + "/" + total + ")");
        }
        System.err.println();

        // Attribute for exact call/type resolution. javac can crash on a few
        // files (e.g. switch-expression AssertionError on JDK 17); isolate and
        // drop those files, then re-attribute the rest so the graph keeps
        // exact edges everywhere else.
        List<Path> crashing = new ArrayList<>();
        if (!tryAnalyze(task, total)) {
            System.err.println();
            System.err.println("WARNING: attribution crashed; isolating offending files...");
            crashing = findCrashingFiles(compiler, fm, files);
            System.err.println("WARNING: excluding " + crashing.size() + " files from attribution: " + crashing);
            files.removeAll(crashing);
            task = newTask(compiler, fm, files);
            units.clear();
            for (var unit : task.parse()) units.add(unit);
            tryAnalyze(task, total);
        }

        var c = new Collector();
        i = 0;
        for (var u : units) {
            c.scan(u, null);
            i++;
            System.err.print("\rScanning: " + (i * 100 / total) + "% (" + i + "/" + total + ")");
        }
        System.err.println();
        c.flush();
    }

    static JavacTask newTask(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> files) {
        return (JavacTask) compiler.getTask(null, fm, null,
                List.of("-proc:none", "-Xlint:none", "-implicit:none",
                        "-XDshouldStopPolicyIfError=ATTR"),
                null, fm.getJavaFileObjectsFromPaths(files));
    }

    static boolean tryAnalyze(JavacTask task, int total) {
        try {
            int i = 0;
            for (var unit : task.analyze()) {
                i++;
                System.err.print("\rAttributing: " + (i * 100 / total) + "% (" + i + "/" + total + ")");
            }
            System.err.println();
            return true;
        } catch (Throwable t) {
            return false;
        }
    }

    /** Binary-search the files that crash javac attribution. */
    static List<Path> findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm, List<Path> files) {
        List<Path> crashing = new ArrayList<>();
        findCrashingFiles(compiler, fm, files, 0, files.size(), crashing);
        return crashing;
    }

    static void findCrashingFiles(JavaCompiler compiler, StandardJavaFileManager fm,
            List<Path> files, int lo, int hi, List<Path> out) {
        if (lo >= hi) return;
        if (hi - lo == 1) {
            out.add(files.get(lo));
            return;
        }
        int mid = (lo + hi) / 2;
        if (chunkCrashes(compiler, fm, files.subList(lo, mid))) {
            findCrashingFiles(compiler, fm, files, lo, mid, out);
        }
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

    static String jstrPath(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    /** Classifies an out-of-project method FQN as stdlib or external. */
    static String categoryOf(String mfqn) {
        if (mfqn == null) return "unknown";
        for (String p : new String[]{"java.", "javax.", "jdk.", "com.sun.", "sun."}) {
            if (mfqn.startsWith(p)) return "stdlib";
        }
        return "external";
    }

    static String stripGenerics(String s) {
        int i = s.indexOf('<');
        return i > 0 ? s.substring(0, i).strip() : s.strip();
    }

    /**
     * Returns the fully-qualified owning class (package.Outer.Inner) for a
     * symbol, walking up past synthetic anonymous/local classes ($-suffixed)
     * to the nearest named type.
     */
    static String ownerFqn(Symbol owner) {
        while (owner instanceof Symbol.ClassSymbol cs) {
            String qn = cs.getQualifiedName().toString();
            if (qn.indexOf('$') < 0) return qn;
            owner = cs.getEnclosingElement();
        }
        return null;
    }

    /**
     * Returns the graph FQN for a method symbol: owning class FQN + method
     * name (constructors use the class simple name to match declarations).
     * Null if not a usable method symbol.
     */
    static String methodFqn(Symbol sym) {
        if (!(sym instanceof Symbol.MethodSymbol ms)) return null;
        String name = ms.getSimpleName().toString();
        String ofqn = ownerFqn(ms.getEnclosingElement());
        if (ofqn == null) return null;
        // Constructors are declared as Class.<init>; match that naming so
        // call targets line up with declarations.
        if (name.equals("<init>")) return ofqn + ".<init>";
        if (name.indexOf('<') >= 0 || name.equals("<error>")) return null;
        return ofqn + "." + name;
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

    /** Symbol attributed onto a call/method-ref expression, or null. */
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

    static class Collector extends TreePathScanner<Void, Void> {
        String pkg = "", cls = "", mtd = "";
        String currentFile = "", sourceText = "";
        final BufferedWriter out = new BufferedWriter(
            new OutputStreamWriter(System.out, StandardCharsets.UTF_8));

        // Complete sets of declared project nodes, used to decide whether a
        // resolved symbol is in-project (call/use) or external (u_call/u_use).
        final Set<String> declaredMethods = new HashSet<>();
        final Set<String> declaredStructs = new HashSet<>();

        // Buffered records resolved in flush() once declarations are complete:
        //   calls:    caller, methodFqn, recvTypeFqn, rawName
        //   typeUses: caller, typeFqn, rawName
        //   classUses: classFqn, typeFqn, rawName  (extends/implements, Struct->Struct)
        final List<String[]> rawCalls = new ArrayList<>();
        final List<String[]> rawTypeUses = new ArrayList<>();
        final List<String[]> rawClassUses = new ArrayList<>();

        void emit(String line) {
            try {
                out.write(line);
                out.newLine();
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
            if (!pkg.isEmpty()) {
                StringBuilder sb = new StringBuilder();
                String prev = "";
                for (int i = 0; i < pkg.length(); i++) {
                    if (pkg.charAt(i) == '.') {
                        String seg = sb.toString();
                        emit("{\"type\":\"pkg\",\"fqn\":\"" + seg + "\"}");
                        if (!prev.isEmpty()) {
                            emit("{\"type\":\"contains\",\"parent\":\"" + prev + "\",\"child\":\"" + seg + "\"}");
                        }
                        prev = seg;
                        sb.append('.');
                    } else {
                        sb.append(pkg.charAt(i));
                        if (i == pkg.length() - 1) {
                            String full = sb.toString();
                            emit("{\"type\":\"pkg\",\"fqn\":\"" + full + "\"}");
                            if (!prev.isEmpty()) {
                                emit("{\"type\":\"contains\",\"parent\":\"" + prev + "\",\"child\":\"" + full + "\"}");
                            }
                        }
                    }
                }
            }
            return super.visitCompilationUnit(cu, nil);
        }

        @Override
        public Void visitClass(ClassTree ct, Void nil) {
            String name = ct.getSimpleName().toString();
            // Anonymous classes: scan their members but attribute them to the
            // enclosing named class (matches javac's $-name collapse).
            if (name.isEmpty()) return super.visitClass(ct, nil);
            String outer = cls;
            cls = cls.isEmpty() ? name : cls + "." + name;
            String fqn = pkg.isEmpty() ? cls : pkg + "." + cls;

            JCTree jc = (JCTree) ct;
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
            // True closing position, not the first brace.
            int bracePos = sourceText.indexOf('{', jc.getStartPosition());
            int end = jc.getEndPosition(jcCu.endPositions);
            if (end <= start) {
                end = bracePos > 0 ? bracePos + 1 : end;
            }

            emit("{\"type\":\"decl\",\"kind\":\"class\",\"fqn\":\"" + fqn
                + "\",\"path\":\"" + jstrPath(currentFile)
                + "\",\"start\":" + start + ",\"end\":" + end + "}");
            declaredStructs.add(fqn);

            String parentFqn = outer.isEmpty() ? pkg : pkg.isEmpty() ? outer : pkg + "." + outer;
            if (!parentFqn.isEmpty()) {
                emit("{\"type\":\"contains\",\"parent\":\"" + parentFqn
                    + "\",\"child\":\"" + fqn + "\"}");
            }

            if (ct.getExtendsClause() != null) {
                recordClassUse(fqn, ct.getExtendsClause());
            }
            for (var iface : ct.getImplementsClause()) {
                recordClassUse(fqn, iface);
            }

            Void r = super.visitClass(ct, nil);
            cls = outer;
            return r;
        }

        @Override
        public Void visitMethod(MethodTree mt, Void nil) {
            String name = mt.getName().toString();
            String fqn = pkg.isEmpty() ? cls + "." + name : pkg + "." + cls + "." + name;
            JCTree jc = (JCTree) mt;
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
            String prevMtd = mtd;
            mtd = fqn;

            emit("{\"type\":\"decl\",\"kind\":\"method\",\"fqn\":\"" + fqn
                + "\",\"path\":\"" + jstrPath(currentFile)
                + "\",\"start\":" + start + ",\"end\":" + end + "}");
            declaredMethods.add(fqn);

            String parentFqn = pkg.isEmpty() ? cls : pkg + "." + cls;
            if (!parentFqn.isEmpty()) {
                emit("{\"type\":\"contains\",\"parent\":\"" + parentFqn
                    + "\",\"child\":\"" + fqn + "\"}");
            }

            Void r = super.visitMethod(mt, nil);
            mtd = prevMtd;
            return r;
        }

        @Override
        public Void visitMethodInvocation(MethodInvocationTree mc, Void nil) {
            if (!mtd.isEmpty()) {
                Symbol sym = symOf(mc);
                Type recv = receiverType(mc.getMethodSelect());
                String recvFqn = recv == null ? null : typeFqnOf(recv);
                rawCalls.add(new String[]{mtd, methodFqn(sym), recvFqn, methodRawName(mc.getMethodSelect())});
            }
            return super.visitMethodInvocation(mc, nil);
        }

        @Override
        public Void visitMemberReference(MemberReferenceTree mref, Void nil) {
            if (!mtd.isEmpty()) {
                Symbol sym = symOf(mref);
                rawCalls.add(new String[]{mtd, methodFqn(sym), null, mref.getName().toString()});
            }
            return super.visitMemberReference(mref, nil);
        }

        @Override
        public Void visitNewClass(NewClassTree nc, Void nil) {
            if (!mtd.isEmpty()) {
                recordTypeUse(mtd, nc.getIdentifier());
                Symbol sym = symOf(nc);
                rawCalls.add(new String[]{mtd, methodFqn(sym), typeFqn(nc.getIdentifier()), typeRawName(nc.getIdentifier())});
            }
            return super.visitNewClass(nc, nil);
        }

        @Override
        public Void visitVariable(VariableTree vt, Void nil) {
            if (!mtd.isEmpty() && vt.getType() != null) recordTypeUse(mtd, vt.getType());
            return super.visitVariable(vt, nil);
        }

        @Override
        public Void visitInstanceOf(InstanceOfTree io, Void nil) {
            if (!mtd.isEmpty() && io.getType() != null) recordTypeUse(mtd, io.getType());
            return super.visitInstanceOf(io, nil);
        }

        @Override
        public Void visitTypeCast(TypeCastTree tc, Void nil) {
            if (!mtd.isEmpty() && tc.getType() != null) recordTypeUse(mtd, tc.getType());
            return super.visitTypeCast(tc, nil);
        }

        void recordTypeUse(String methodFqn, Tree type) {
            rawTypeUses.add(new String[]{methodFqn, typeFqn(type), typeRawName(type)});
        }

        void recordClassUse(String classFqn, Tree type) {
            rawClassUses.add(new String[]{classFqn, typeFqn(type), typeRawName(type)});
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

        void flush() {
            try {
                for (String[] rc : rawCalls) {
                    String caller = rc[0], mfqn = rc[1], recvFqn = rc[2], rawName = rc[3];
                    if (mfqn != null && declaredMethods.contains(mfqn)) {
                        emit("{\"type\":\"call\",\"source\":\"" + caller
                            + "\",\"target\":\"" + mfqn + "\"}");
                    } else if (recvFqn != null && declaredStructs.contains(recvFqn)) {
                        emit("{\"type\":\"use\",\"source\":\"" + caller
                            + "\",\"target\":\"" + recvFqn + "\"}");
                    } else {
                        String target = mfqn != null ? mfqn : rawName;
                        emit("{\"type\":\"u_call\",\"source\":\"" + caller
                            + "\",\"target\":\"" + (target == null ? "?" : target)
                            + "\",\"category\":\"" + categoryOf(mfqn) + "\"}");
                    }
                }

                for (String[] tu : rawTypeUses) {
                    String caller = tu[0], tfqn = tu[1], rawName = tu[2];
                    if (tfqn != null && declaredStructs.contains(tfqn)) {
                        emit("{\"type\":\"use\",\"source\":\"" + caller
                            + "\",\"target\":\"" + tfqn + "\"}");
                    } else if (tfqn == null) {
                        emit("{\"type\":\"u_use\",\"source\":\"" + caller
                            + "\",\"target\":\"" + (rawName == null ? "?" : rawName) + "\"}");
                    }
                    // external (resolved but not in-project) types are dropped:
                    // ubiquitous, low-signal, and were the fixture-collision source.
                }

                for (String[] cu : rawClassUses) {
                    String caller = cu[0], tfqn = cu[1], rawName = cu[2];
                    if (tfqn != null && declaredStructs.contains(tfqn)) {
                        emit("{\"type\":\"use\",\"source\":\"" + caller
                            + "\",\"target\":\"" + tfqn + "\"}");
                    } else if (tfqn == null) {
                        emit("{\"type\":\"u_use\",\"source\":\"" + caller
                            + "\",\"target\":\"" + (rawName == null ? "?" : rawName) + "\"}");
                    }
                }

                out.flush();
                System.err.println();
            } catch (IOException e) {
                throw new RuntimeException(e);
            }
        }
    }
}
