import com.sun.source.tree.*;
import com.sun.source.util.*;
import com.sun.tools.javac.tree.JCTree;
import javax.tools.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

public class CallGraphBuilder {
    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        List<Path> files = new ArrayList<>();
        try (var walk = Files.walk(dir)) {
            walk.filter(p -> p.toString().endsWith(".java")).filter(Files::isRegularFile).forEach(files::add);
        }

        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        var fm = compiler.getStandardFileManager(null, null, null);
        var task = (JavacTask) compiler.getTask(null, fm, null,
                List.of("-proc:none", "-Xlint:none", "-implicit:none"),
                null, fm.getJavaFileObjectsFromPaths(files));

        int total = files.size();
        var units = new ArrayList<CompilationUnitTree>();
        int i = 0;
        for (var unit : task.parse()) {
            units.add(unit);
            i++;
            System.err.print("\rParsing: " + (i * 100 / total) + "% (" + i + "/" + total + ")");
        }
        System.err.println();

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

    static String jstrPath(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    static String stripGenerics(String s) {
        int i = s.indexOf('<');
        return i > 0 ? s.substring(0, i).strip() : s.strip();
    }

    static class Collector extends TreePathScanner<Void, Void> {
        String pkg = "", cls = "", mtd = "", mtdKey = "";
        String currentFile = "", sourceText = "";

        Map<String, List<String>> byKey = new HashMap<>();
        Map<String, List<String>> simpleToFqn = new HashMap<>();
        List<String[]> rawCalls = new ArrayList<>();
        List<String[]> rawExtends = new ArrayList<>();
        List<String[]> rawImplements = new ArrayList<>();
        List<String[]> rawTypeUses = new ArrayList<>();

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
                        System.out.println("{\"type\":\"pkg\",\"fqn\":\"" + seg + "\"}");
                        if (!prev.isEmpty()) {
                            System.out.println("{\"type\":\"contains\",\"parent\":\"" + prev + "\",\"child\":\"" + seg + "\"}");
                        }
                        prev = seg;
                        sb.append('.');
                    } else {
                        sb.append(pkg.charAt(i));
                        if (i == pkg.length() - 1) {
                            String full = sb.toString();
                            System.out.println("{\"type\":\"pkg\",\"fqn\":\"" + full + "\"}");
                            if (!prev.isEmpty()) {
                                System.out.println("{\"type\":\"contains\",\"parent\":\"" + prev + "\",\"child\":\"" + full + "\"}");
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
            int bracePos = sourceText.indexOf('{', jc.getStartPosition());
            int end = bracePos > 0 ? bracePos + 1 : jc.getEndPosition(jcCu.endPositions);

            System.out.println("{\"type\":\"decl\",\"kind\":\"class\",\"fqn\":\"" + fqn
                + "\",\"path\":\"" + jstrPath(currentFile)
                + "\",\"start\":" + start + ",\"end\":" + end + "}");

            String parentFqn = outer.isEmpty() ? pkg : pkg.isEmpty() ? outer : pkg + "." + outer;
            if (!parentFqn.isEmpty()) {
                System.out.println("{\"type\":\"contains\",\"parent\":\"" + parentFqn
                    + "\",\"child\":\"" + fqn + "\"}");
            }

            String simple = fqn.contains(".") ? fqn.substring(fqn.lastIndexOf('.') + 1) : fqn;
            simpleToFqn.computeIfAbsent(simple, k -> new ArrayList<>()).add(fqn);

            if (ct.getExtendsClause() != null) {
                rawExtends.add(new String[]{fqn, stripGenerics(ct.getExtendsClause().toString())});
            }
            for (var iface : ct.getImplementsClause()) {
                rawImplements.add(new String[]{fqn, stripGenerics(iface.toString())});
            }

            Void r = super.visitClass(ct, nil);
            cls = outer;
            return r;
        }

        @Override
        public Void visitMethod(MethodTree mt, Void nil) {
            String name = mt.getName().toString();
            int argc = mt.getParameters().size();
            String fqn = pkg.isEmpty() ? cls + "." + name : pkg + "." + cls + "." + name;
            String key = name + "#" + argc;
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
            String prevKey = mtdKey;
            mtd = fqn;
            mtdKey = key;

            System.out.println("{\"type\":\"decl\",\"kind\":\"method\",\"fqn\":\"" + fqn
                + "\",\"path\":\"" + jstrPath(currentFile)
                + "\",\"start\":" + start + ",\"end\":" + end + "}");

            String parentFqn = pkg.isEmpty() ? cls : pkg + "." + cls;
            if (!parentFqn.isEmpty()) {
                System.out.println("{\"type\":\"contains\",\"parent\":\"" + parentFqn
                    + "\",\"child\":\"" + fqn + "\"}");
            }

            byKey.computeIfAbsent(key, k -> new ArrayList<>()).add(fqn);

            String simple = fqn.contains(".") ? fqn.substring(fqn.lastIndexOf('.') + 1) : fqn;
            simpleToFqn.computeIfAbsent(simple, k -> new ArrayList<>()).add(fqn);

            Void r = super.visitMethod(mt, nil);
            mtd = prevMtd;
            mtdKey = prevKey;
            return r;
        }

        @Override
        public Void visitMethodInvocation(MethodInvocationTree mc, Void nil) {
            if (!mtd.isEmpty()) {
                ExpressionTree sel = mc.getMethodSelect();
                String name = sel instanceof MemberSelectTree ms
                        ? ms.getIdentifier().toString()
                        : sel.toString();
                rawCalls.add(new String[]{mtd, name + "#" + mc.getArguments().size()});
            }
            return super.visitMethodInvocation(mc, nil);
        }

        @Override
        public Void visitVariable(VariableTree vt, Void nil) {
            if (!mtd.isEmpty() && vt.getType() != null) collectTypes(mtd, vt.getType());
            return super.visitVariable(vt, nil);
        }

        @Override
        public Void visitNewClass(NewClassTree nc, Void nil) {
            if (!mtd.isEmpty()) collectTypes(mtd, nc.getIdentifier());
            return super.visitNewClass(nc, nil);
        }

        @Override
        public Void visitInstanceOf(InstanceOfTree io, Void nil) {
            if (!mtd.isEmpty()) collectTypes(mtd, io.getType());
            return super.visitInstanceOf(io, nil);
        }

        @Override
        public Void visitTypeCast(TypeCastTree tc, Void nil) {
            if (!mtd.isEmpty()) collectTypes(mtd, tc.getType());
            return super.visitTypeCast(tc, nil);
        }

        void collectTypes(String methodFqn, Tree type) {
            if (type instanceof IdentifierTree id) {
                rawTypeUses.add(new String[]{methodFqn, id.getName().toString()});
            } else if (type instanceof MemberSelectTree ms) {
                rawTypeUses.add(new String[]{methodFqn, ms.getIdentifier().toString()});
            } else if (type instanceof ParameterizedTypeTree pt) {
                collectTypes(methodFqn, pt.getType());
                for (var ta : pt.getTypeArguments()) collectTypes(methodFqn, ta);
            } else if (type instanceof ArrayTypeTree at) {
                collectTypes(methodFqn, at.getType());
            }
        }

        void flush() {
            int flushTotal = rawCalls.size() + rawTypeUses.size() + rawExtends.size() + rawImplements.size();
            int flushDone = 0;
            int lastTick = -1;

            try {
                var buf = new BufferedWriter(new OutputStreamWriter(System.out, StandardCharsets.UTF_8));

                Iterator<String[]> it = rawCalls.iterator();
                while (it.hasNext()) {
                    String[] rc = it.next();
                    String caller = rc[0], targetKey = rc[1];
                    List<String> cands = byKey.get(targetKey);
                    it.remove();
                    if (cands != null) {
                        String callerClass = caller.contains(".")
                                ? caller.substring(0, caller.lastIndexOf('.')) : "";
                        String best = cands.get(0);
                        for (String c : cands) {
                            if (c.startsWith(callerClass + ".")) { best = c; break; }
                        }
                        buf.write("{\"type\":\"call\",\"source\":\"" + caller
                            + "\",\"target\":\"" + best + "\"}");
                        buf.newLine();
                    }
                    flushDone++;
                    int tick = (int)((long)flushDone * 1000 / flushTotal);
                    if (tick != lastTick) {
                        lastTick = tick;
                        System.err.print("\rResolving: " + (tick / 10) + "." + (tick % 10) + "% (" + flushDone + "/" + flushTotal + ")");
                    }
                }

                it = rawTypeUses.iterator();
                while (it.hasNext()) {
                    String[] tu = it.next();
                    String fqn = resolveSimple(tu[0], tu[1], simpleToFqn);
                    it.remove();
                    if (fqn != null) {
                        buf.write("{\"type\":\"use\",\"source\":\"" + tu[0]
                            + "\",\"target\":\"" + fqn + "\"}");
                        buf.newLine();
                    }
                    if (++flushDone % 10000 == 0) buf.flush();
                    int tick = (int)((long)flushDone * 1000 / flushTotal);
                    if (tick != lastTick) {
                        lastTick = tick;
                        System.err.print("\rResolving: " + (tick / 10) + "." + (tick % 10) + "% (" + flushDone + "/" + flushTotal + ")");
                    }
                }

                it = rawExtends.iterator();
                while (it.hasNext()) {
                    String[] re = it.next();
                    String fqn = resolveSimple(re[0], re[1], simpleToFqn);
                    it.remove();
                    if (fqn != null) {
                        buf.write("{\"type\":\"use\",\"source\":\"" + re[0]
                            + "\",\"target\":\"" + fqn + "\"}");
                        buf.newLine();
                    }
                    if (++flushDone % 10000 == 0) buf.flush();
                    int tick = (int)((long)flushDone * 1000 / flushTotal);
                    if (tick != lastTick) {
                        lastTick = tick;
                        System.err.print("\rResolving: " + (tick / 10) + "." + (tick % 10) + "% (" + flushDone + "/" + flushTotal + ")");
                    }
                }

                it = rawImplements.iterator();
                while (it.hasNext()) {
                    String[] ri = it.next();
                    String fqn = resolveSimple(ri[0], ri[1], simpleToFqn);
                    it.remove();
                    if (fqn != null) {
                        buf.write("{\"type\":\"use\",\"source\":\"" + ri[0]
                            + "\",\"target\":\"" + fqn + "\"}");
                        buf.newLine();
                    }
                    if (++flushDone % 10000 == 0) buf.flush();
                    int tick = (int)((long)flushDone * 1000 / flushTotal);
                    if (tick != lastTick) {
                        lastTick = tick;
                        System.err.print("\rResolving: " + (tick / 10) + "." + (tick % 10) + "% (" + flushDone + "/" + flushTotal + ")");
                    }
                }

                buf.flush();
                System.err.println();
            } catch (IOException e) {
                throw new RuntimeException(e);
            }
        }

        String resolveSimple(String fromFqn, String simple, Map<String, List<String>> map) {
            List<String> cands = map.get(simple);
            if (cands == null || cands.isEmpty()) return null;
            if (cands.size() == 1) return cands.get(0);
            String fromPkg = fromFqn.contains(".")
                    ? fromFqn.substring(0, fromFqn.lastIndexOf('.')) : "";
            for (String c : cands) {
                String cPkg = c.contains(".")
                        ? c.substring(0, c.lastIndexOf('.')) : "";
                if (cPkg.equals(fromPkg)) return c;
            }
            return cands.get(0);
        }
    }
}
