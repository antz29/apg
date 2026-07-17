import com.sun.source.tree.*;
import com.sun.source.util.*;
import com.sun.tools.javac.tree.JCTree;
import javax.tools.*;
import javax.lang.model.element.ExecutableElement;
import javax.lang.model.element.TypeElement;
import javax.lang.model.type.*;
import javax.xml.parsers.*;
import org.w3c.dom.*;
import org.xml.sax.helpers.DefaultHandler;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

public class CallGraphBuilder {
    public static void main(String[] args) throws Exception {
        String classpath = null;
        Path dir = null;
        List<String> excludePaths = new ArrayList<>();

        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--classpath" -> { if (++i < args.length) classpath = args[i]; }
                case "--exclude-path" -> { if (++i < args.length) excludePaths.add(args[i]); }
                default -> { if (dir == null) dir = Paths.get(args[i]); }
            }
        }
        if (dir == null) dir = Paths.get("project");
        if (classpath == null) classpath = System.getenv("APG_CLASSPATH");
        if (classpath == null) {
                System.err.print("\r  Auto-detecting classpath...");
                classpath = autoDetectClasspath(dir);
                if (classpath == null) System.err.println(" none found");
                else System.err.println(" " + classpath.split(File.pathSeparator).length + " entries");
        }

        List<Path> files = new ArrayList<>();
        System.err.print("\r  Scanning files...");
        try (var walk = Files.walk(dir)) {
            walk.filter(p -> p.toString().endsWith(".java"))
                .filter(Files::isRegularFile)
                .filter(p -> excludePaths.stream().noneMatch(pat -> p.toString().contains(pat)))
                .forEach(p -> {
                    files.add(p);
                    if (files.size() % 5000 == 0) System.err.print("\r  Scanning files... " + files.size());
                });
        }
        System.err.println("\r  Scanning files... " + files.size() + " found");

        JavaCompiler compiler = ToolProvider.getSystemJavaCompiler();
        var fm = compiler.getStandardFileManager(null, null, null);

        if (classpath != null) {
            int count = classpath.split(File.pathSeparator).length;
            System.err.println("Classpath: " + count + " entries");
        } else {
            System.err.println("Warning: no classpath available — external types will not be resolved");
        }

        List<String> options = new ArrayList<>(List.of("-proc:none", "-Xlint:none", "-implicit:none"));
        if (classpath != null && !classpath.isEmpty()) {
            options.add("-classpath");
            options.add(classpath);
        }

        var task = (JavacTask) compiler.getTask(null, fm, null, options, null,
                fm.getJavaFileObjectsFromPaths(files));

        int total = files.size();
        int barWidth = 30;

        // Parse
        var units = new ArrayList<CompilationUnitTree>();
        {
            int done = 0;
            for (var unit : task.parse()) {
                units.add(unit);
                progressBar("Parsing", ++done, total, barWidth);
            }
            System.err.println();
        }

        // Trees instance — context is alive before analyze
        var trees = Trees.instance(task);

        // Analyze (type-check) — enters symbols + attributes types
        System.err.println("  Analyzing...");
        task.analyze();
        System.err.println("  Analyzing done");

        // Scan and emit with Trees API (AST nodes retain type info after call())
        var c = new Collector();
        c.trees = trees;
        c.buf = new BufferedWriter(new OutputStreamWriter(System.out, StandardCharsets.UTF_8));
        {
            int done = 0;
            for (var u : units) {
                c.scan(u, null);
                progressBar("Emitting", ++done, total, barWidth);
            }
            System.err.println();
        }
        c.buf.flush();
    }

    static String jstrPath(String s) {
        return s.replace("\\", "\\\\").replace("\"", "\\\"");
    }

    static String autoDetectClasspath(Path dir) {
        Set<String> entries = new LinkedHashSet<>();

        String m2Repo = Paths.get(System.getProperty("user.home"), ".m2", "repository").toString();

        // Phase 1: parse all pom.xml files for Maven dependencies
        List<Path> poms = new ArrayList<>();
        try (var walk = Files.walk(dir)) {
            walk.filter(p -> p.endsWith("pom.xml")).forEach(poms::add);
        } catch (IOException ignored) {}
        if (!poms.isEmpty()) {
            Set<Path> parsed = new HashSet<>();
            for (var pom : poms) {
                resolvePomDeps(pom, m2Repo, entries, parsed);
            }
        }

        // Phase 2: scan for compiled artifact directories and jars
        try (var walk = Files.walk(dir, 20)) {
            walk.forEach(p -> {
                String s = p.toString();
                if (Files.isDirectory(p)) {
                    if (s.endsWith("target/classes") || s.endsWith("build/classes")) {
                        entries.add(p.toAbsolutePath().toString());
                    }
                } else if (s.endsWith(".jar")) {
                    Path parent = p.getParent();
                    if (parent == null) return;
                    String ps = parent.toString();
                    if (ps.endsWith("target/dependency") || ps.endsWith("target/lib")
                        || ps.endsWith("build/libs") || ps.endsWith("build/lib")
                        || ps.equals("lib") || ps.equals("libs")) {
                        entries.add(p.toAbsolutePath().toString());
                    }
                }
            });
        } catch (IOException ignored) {}

        if (entries.isEmpty()) return null;

        StringBuilder sb = new StringBuilder();
        for (var e : entries) {
            if (sb.length() > 0) sb.append(File.pathSeparatorChar);
            sb.append(e);
        }
        return sb.toString();
    }

    static void resolvePomDeps(Path pom, String m2Repo, Set<String> entries, Set<Path> parsed) {
        if (!parsed.add(pom)) return;
        try {
            var factory = DocumentBuilderFactory.newInstance();
            factory.setFeature("http://apache.org/xml/features/nonvalidating/load-external-dtd", false);
            var builder = factory.newDocumentBuilder();
            builder.setErrorHandler(new DefaultHandler());
            Document doc = builder.parse(pom.toFile());
            doc.getDocumentElement().normalize();

            Map<String, String> props = new HashMap<>();
            NodeList propNodes = doc.getElementsByTagName("properties");
            if (propNodes.getLength() > 0) {
                NodeList children = propNodes.item(0).getChildNodes();
                for (int i = 0; i < children.getLength(); i++) {
                    Node c = children.item(i);
                    if (c.getNodeType() == Node.ELEMENT_NODE) {
                        props.put(c.getNodeName(), c.getTextContent().trim());
                    }
                }
            }
            putIfPresent(props, "project.groupId", doc, "groupId");
            putIfPresent(props, "project.artifactId", doc, "artifactId");
            putIfPresent(props, "project.version", doc, "version");

            // dependency management map
            Map<String, String> depMgmt = new HashMap<>();
            NodeList mgmtNodes = doc.getElementsByTagName("dependencyManagement");
            if (mgmtNodes.getLength() > 0) {
                NodeList deps = ((Element) mgmtNodes.item(0)).getElementsByTagName("dependency");
                for (int i = 0; i < deps.getLength(); i++) {
                    addDep(depMgmt, (Element) deps.item(i), props);
                }
            }

            // actual dependencies
            NodeList depNodes = doc.getElementsByTagName("dependency");
            for (int i = 0; i < depNodes.getLength(); i++) {
                Element dep = (Element) depNodes.item(i);
                Node pn = dep.getParentNode();
                if (pn != null && pn.getNodeName().equals("dependencyManagement")) continue;

                String scope = getText(dep, "scope");
                if (scope != null && (scope.equals("test") || scope.equals("provided"))) continue;

                String g = getText(dep, "groupId");
                String a = getText(dep, "artifactId");
                String v = getText(dep, "version");
                if (v == null) v = depMgmt.get(g + ":" + a);
                if (v != null) v = resolveProps(v, props);

                if (g != null && a != null && v != null) {
                    String jar = m2Repo + "/" + g.replace('.', '/') + "/" + a + "/" + v + "/" + a + "-" + v + ".jar";
                    if (Files.exists(Paths.get(jar))) entries.add(jar);
                    // also try sources or classes dir
                    String classesDir = pom.getParent().resolve("target/classes").toAbsolutePath().toString();
                    if (Files.exists(Paths.get(classesDir))) entries.add(classesDir);
                }
            }

            // recurse into parent POM
            NodeList parentNodes = doc.getElementsByTagName("parent");
            if (parentNodes.getLength() > 0) {
                Element pe = (Element) parentNodes.item(0);
                String pg = getText(pe, "groupId");
                String pa = getText(pe, "artifactId");
                String pv = getText(pe, "version");
                if (pv != null) pv = resolveProps(pv, props);
                if (pg != null && pa != null && pv != null) {
                    Path parentPom = Paths.get(m2Repo, pg.replace('.', '/'), pa, pv, pa + "-" + pv + ".pom");
                    if (Files.exists(parentPom)) {
                        resolvePomDeps(parentPom, m2Repo, entries, parsed);
                    }
                }
            }
        } catch (Exception ignored) {}
    }

    static void putIfPresent(Map<String, String> map, String key, Document doc, String tag) {
        NodeList nl = doc.getElementsByTagName(tag);
        if (nl.getLength() > 0) map.put(key, nl.item(0).getTextContent().trim());
    }

    static void addDep(Map<String, String> map, Element dep, Map<String, String> props) {
        String g = getText(dep, "groupId");
        String a = getText(dep, "artifactId");
        String v = getText(dep, "version");
        if (v != null) v = resolveProps(v, props);
        if (g != null && a != null && v != null) map.put(g + ":" + a, v);
    }

    static String getText(Element parent, String tag) {
        NodeList nl = parent.getElementsByTagName(tag);
        return nl.getLength() > 0 ? nl.item(0).getTextContent().trim() : null;
    }

    static String resolveProps(String s, Map<String, String> props) {
        if (s == null) return null;
        int si = s.indexOf("${");
        if (si < 0) return s;
        int ei = s.indexOf("}", si);
        if (ei < 0) return s;
        String key = s.substring(si + 2, ei);
        String val = props.get(key);
        if (val == null) val = System.getProperty(key);
        if (val == null) val = System.getenv(key);
        if (val == null) return s;
        String r = s.substring(0, si) + val + s.substring(ei + 1);
        return resolveProps(r, props);
    }

    static void progressBar(String label, int done, int total, int width) {
        int pct = done * 100 / total;
        int filled = pct * width / 100;
        StringBuilder sb = new StringBuilder("\r  ");
        sb.append(label).append(" [");
        for (int i = 0; i < width; i++) sb.append(i < filled ? '=' : i == filled && filled < width ? '>' : ' ');
        sb.append("] ").append(pct).append("% (").append(done).append("/").append(total).append(")");
        System.err.print(sb);
        System.err.flush();
    }

    static class Collector extends TreePathScanner<Void, Void> {
        String pkg = "", cls = "", mtd = "";
        String currentFile = "", sourceText = "";
        Trees trees;
        BufferedWriter buf;

        void emit(String line) throws IOException {
            buf.write(line);
            buf.newLine();
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
                        try { emit("{\"type\":\"pkg\",\"fqn\":\"" + seg + "\"}"); } catch (IOException e) { throw new RuntimeException(e); }
                        if (!prev.isEmpty()) {
                            try { emit("{\"type\":\"contains\",\"parent\":\"" + prev + "\",\"child\":\"" + seg + "\"}"); } catch (IOException e) { throw new RuntimeException(e); }
                        }
                        prev = seg;
                        sb.append('.');
                    } else {
                        sb.append(pkg.charAt(i));
                        if (i == pkg.length() - 1) {
                            String full = sb.toString();
                            try { emit("{\"type\":\"pkg\",\"fqn\":\"" + full + "\"}"); } catch (IOException e) { throw new RuntimeException(e); }
                            if (!prev.isEmpty()) {
                                try { emit("{\"type\":\"contains\",\"parent\":\"" + prev + "\",\"child\":\"" + full + "\"}"); } catch (IOException e) { throw new RuntimeException(e); }
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

            try {
                emit("{\"type\":\"decl\",\"kind\":\"class\",\"fqn\":\"" + fqn
                    + "\",\"path\":\"" + jstrPath(currentFile)
                    + "\",\"start\":" + start + ",\"end\":" + end + "}");

                String parentFqn = outer.isEmpty() ? pkg : pkg.isEmpty() ? outer : pkg + "." + outer;
                if (!parentFqn.isEmpty()) {
                    emit("{\"type\":\"contains\",\"parent\":\"" + parentFqn
                        + "\",\"child\":\"" + fqn + "\"}");
                }

                if (ct.getExtendsClause() != null) {
                    TreePath extPath = new TreePath(getCurrentPath(), ct.getExtendsClause());
                    TypeMirror tm = trees.getTypeMirror(extPath);
                    if (tm != null) collectTypeUses(fqn, tm);
                }
                for (var iface : ct.getImplementsClause()) {
                    TreePath implPath = new TreePath(getCurrentPath(), iface);
                    TypeMirror tm = trees.getTypeMirror(implPath);
                    if (tm != null) collectTypeUses(fqn, tm);
                }
            } catch (IOException e) {
                throw new RuntimeException(e);
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

            try {
                emit("{\"type\":\"decl\",\"kind\":\"method\",\"fqn\":\"" + fqn
                    + "\",\"path\":\"" + jstrPath(currentFile)
                    + "\",\"start\":" + start + ",\"end\":" + end + "}");

                String parentFqn = pkg.isEmpty() ? cls : pkg + "." + cls;
                if (!parentFqn.isEmpty()) {
                    emit("{\"type\":\"contains\",\"parent\":\"" + parentFqn
                        + "\",\"child\":\"" + fqn + "\"}");
                }

                if (mt.getReturnType() != null) {
                    TypeMirror tm = trees.getTypeMirror(new TreePath(getCurrentPath(), mt.getReturnType()));
                    if (tm != null) collectTypeUses(fqn, tm);
                }
            } catch (IOException e) {
                throw new RuntimeException(e);
            }

            Void r = super.visitMethod(mt, nil);
            mtd = prevMtd;
            return r;
        }

        @Override
        public Void visitMethodInvocation(MethodInvocationTree mc, Void nil) {
            if (!mtd.isEmpty()) {
                javax.lang.model.element.Element el = trees.getElement(getCurrentPath());
                if (el instanceof ExecutableElement ee) {
                    String target = ((TypeElement) ee.getEnclosingElement()).getQualifiedName()
                        + "." + ee.getSimpleName();
                    try {
                        emit("{\"type\":\"call\",\"source\":\"" + mtd
                            + "\",\"target\":\"" + target + "\"}");
                    } catch (IOException e) {
                        throw new RuntimeException(e);
                    }
                }
            }
            return super.visitMethodInvocation(mc, nil);
        }

        @Override
        public Void visitVariable(VariableTree vt, Void nil) {
            if (!mtd.isEmpty() && vt.getType() != null) {
                TypeMirror tm = trees.getTypeMirror(new TreePath(getCurrentPath(), vt.getType()));
                if (tm != null) {
                    try { collectTypeUses(mtd, tm); } catch (IOException e) { throw new RuntimeException(e); }
                }
            }
            return super.visitVariable(vt, nil);
        }

        @Override
        public Void visitNewClass(NewClassTree nc, Void nil) {
            if (!mtd.isEmpty()) {
                TypeMirror tm = trees.getTypeMirror(new TreePath(getCurrentPath(), nc.getIdentifier()));
                if (tm != null) {
                    try { collectTypeUses(mtd, tm); } catch (IOException e) { throw new RuntimeException(e); }
                }
            }
            return super.visitNewClass(nc, nil);
        }

        @Override
        public Void visitInstanceOf(InstanceOfTree io, Void nil) {
            if (!mtd.isEmpty()) {
                TypeMirror tm = trees.getTypeMirror(new TreePath(getCurrentPath(), io.getType()));
                if (tm != null) {
                    try { collectTypeUses(mtd, tm); } catch (IOException e) { throw new RuntimeException(e); }
                }
            }
            return super.visitInstanceOf(io, nil);
        }

        @Override
        public Void visitTypeCast(TypeCastTree tc, Void nil) {
            if (!mtd.isEmpty()) {
                TypeMirror tm = trees.getTypeMirror(new TreePath(getCurrentPath(), tc.getType()));
                if (tm != null) {
                    try { collectTypeUses(mtd, tm); } catch (IOException e) { throw new RuntimeException(e); }
                }
            }
            return super.visitTypeCast(tc, nil);
        }

        void collectTypeUses(String sourceFqn, TypeMirror tm) throws IOException {
            if (tm instanceof DeclaredType dt) {
                javax.lang.model.element.Element el = dt.asElement();
                if (el instanceof TypeElement te) {
                    emit("{\"type\":\"use\",\"source\":\"" + sourceFqn
                        + "\",\"target\":\"" + te.getQualifiedName() + "\"}");
                }
                for (TypeMirror ta : dt.getTypeArguments()) {
                    collectTypeUses(sourceFqn, ta);
                }
            } else if (tm instanceof ArrayType at) {
                collectTypeUses(sourceFqn, at.getComponentType());
            }
        }
    }
}
