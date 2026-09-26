import com.sun.source.tree.*;
import com.sun.source.util.*;
import javax.lang.model.element.*;
import javax.lang.model.type.*;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

class Collector extends TreePathScanner<Void, Void> {
    String pkg = "", cls = "", mtd = "";
    String currentFile = "", sourceText = "";
    boolean emitting = false;
    final JsonlWriter writer = new JsonlWriter();

    // Opaque ids (SPEC §3): a single monotonic counter across structs and
    // methods. structID/funcID map canonical keys to ids; treeID records
    // which tree won a key, so a deduped (anonymous/local-collision)
    // declaration is skipped at emit time rather than causing a residual
    // FQN collision in the ingestor.
    int nextId = 0;
    final Map<String, String> structID = new HashMap<>();
    final Map<String, String> funcID = new HashMap<>();
    final IdentityHashMap<Tree, String> treeID = new IdentityHashMap<>();

    final Set<String> emittedPkg = new HashSet<>();
    final String idPrefix;
    Trees trees;
    SourcePositions sourcePos;
    DocTrees docTrees;

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
        this.surfaceSimpleStruct = DeclarationSurface.simpleStructIndex(surfaceStructs);
        this.surfaceCtorBySimple = DeclarationSurface.constructorIndex(surfaceFuncFqn);
    }

    /** Attach the public-API attribution services for this task (call after analyze). */
    void useTask(JavacTask task) {
        this.trees = Trees.instance(task);
        this.sourcePos = trees.getSourcePositions();
        this.docTrees = DocTrees.instance(task);
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
            m.putAll(DeclarationSurface.simpleStructIndex(structID.keySet()));
            projectStructBySimpleCache = m;
        }
        return projectStructBySimpleCache;
    }

    /** Simple class name -> canonical `<init>` FQN over surface UNION target declarations. */
    Map<String, String> projectCtorBySimple() {
        if (projectCtorBySimpleCache == null) {
            Map<String, String> m = new HashMap<>(surfaceCtorBySimple);
            m.putAll(DeclarationSurface.constructorIndex(SymbolNaming.renderedFuncFqn(funcID)));
            projectCtorBySimpleCache = m;
        }
        return projectCtorBySimpleCache;
    }

    String newNodeID() {
        return idPrefix + (++nextId);
    }

    void emitPkgHierarchy(String pkg) {
        // The empty (default) package is NOT a no-op: moduleIdentityFor
        // maps it to the non-empty default-package identity, so its
        // sources get a real root module (and, via the ingestor's language
        // rooting, an all-default-package scan still materialises the
        // `java` Language root). A packaged input is unchanged.
        String identity = SymbolNaming.moduleIdentityFor(pkg);
        String prev = "";
        String cur = "";
        for (String seg : identity.split("\\.")) {
            cur = cur.isEmpty() ? seg : cur + "." + seg;
            if (!emittedPkg.contains(cur)) {
                emittedPkg.add(cur);
                writer.emit("{\"type\":\"module\",\"fqn\":\"" + JsonlWriter.jstr(cur) + "\"}");
            }
            if (!prev.isEmpty()) {
                writer.emitEdge("contains", prev, cur);
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
            Progress.progress("Collecting", i, total, SourceDiscovery.fileBase(u));
        }
        Progress.endProgress();
    }

    void emitAll(List<CompilationUnitTree> units, int total) {
        emitting = true;
        int i = 0;
        for (var u : units) {
            scan(u, null);
            i++;
            Progress.progress("Emit", i, total, SourceDiscovery.fileBase(u));
        }
        Progress.endProgress();
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
            writer.emit("{\"type\":\"file\",\"path\":\"" + JsonlWriter.jstr(currentFile)
                + "\",\"parent\":\"" + JsonlWriter.jstr(SymbolNaming.fileParentFor(pkg))
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
        writer.emit("{\"type\":\"file\",\"path\":\"" + JsonlWriter.jstr(abs.toString())
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
        String fqn = SymbolNaming.classFqnFor(pkg, cls);

        if (!emitting) {
            if (!structID.containsKey(fqn)) {
                String id = newNodeID();
                structID.put(fqn, id);
                treeID.put(ct, id);
            }
        } else {
            String id = treeID.get(ct);
            if (id != null) {
                String parentFqn = SymbolNaming.classParentFor(pkg, outer);
                int[] span = classSpan(ct);
                writer.emit("{\"type\":\"struct\",\"id\":\"" + JsonlWriter.jstr(id)
                    + "\",\"parent\":\"" + JsonlWriter.jstr(parentFqn)
                    + "\",\"name\":\"" + JsonlWriter.jstr(name)
                    + "\",\"path\":\"" + JsonlWriter.jstr(currentFile)
                    + "\",\"start\":" + span[0] + ",\"end\":" + span[1]
                    + ",\"start_line\":" + lineOf(span[0])
                    + ",\"end_line\":" + lineOf(span[1] - 1) + "}");
                // Nested classes stay directly under their outer class;
                // top-level classes are reached through the file node
                // instead of the package (SPEC §7).
                if (structID.containsKey(parentFqn)) {
                    writer.emitEdge("contains", structID.get(parentFqn), id);
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
        List<String> params = SymbolNaming.paramStrings(SymbolNaming.symOfDecl(trees, getCurrentPath(), mt));
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
                int[] span = methodSpan(mt);
                StringBuilder paramsJson = new StringBuilder();
                for (String p : params) {
                    if (paramsJson.length() > 0) paramsJson.append(',');
                    paramsJson.append('"').append(JsonlWriter.jstr(p)).append('"');
                }
                writer.emit("{\"type\":\"function\",\"id\":\"" + JsonlWriter.jstr(myId)
                    + "\",\"parent\":\"" + JsonlWriter.jstr(parentFqn)
                    + "\",\"name\":\"" + JsonlWriter.jstr(name)
                    + "\",\"params\":[" + paramsJson + "]"
                    + ",\"file\":\"" + JsonlWriter.jstr(currentFile)
                    + "\",\"path\":\"" + JsonlWriter.jstr(currentFile)
                    + "\",\"start\":" + span[0] + ",\"end\":" + span[1]
                    + ",\"start_line\":" + lineOf(span[0])
                    + ",\"end_line\":" + lineOf(span[1] - 1) + "}");
                if (structID.containsKey(parentFqn)) {
                    writer.emitEdge("contains", structID.get(parentFqn), myId);
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
            Element sym = SymbolNaming.symOf(trees, getCurrentPath(), mc);
            TypeMirror recv = receiverType(mc.getMethodSelect());
            resolveCall(sym, recv == null ? null : typeFqnOf(recv), SymbolNaming.methodRawName(mc.getMethodSelect()));
        }
        return super.visitMethodInvocation(mc, nil);
    }

    @Override
    public Void visitMemberReference(MemberReferenceTree mref, Void nil) {
        if (!mtd.isEmpty()) {
            Element sym = SymbolNaming.symOf(trees, getCurrentPath(), mref);
            resolveCall(sym, null, mref.getName().toString());
        }
        return super.visitMemberReference(mref, nil);
    }

    @Override
    public Void visitNewClass(NewClassTree nc, Void nil) {
        if (!mtd.isEmpty()) {
            recordUse(mtd, nc.getIdentifier());
            Element sym = SymbolNaming.symOf(trees, getCurrentPath(), nc);
            resolveCall(sym, SymbolNaming.typeFqn(nc.getIdentifier(), trees, getCurrentPath()), SymbolNaming.typeRawName(nc.getIdentifier()));
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
    void resolveCall(Element sym, String recvFqn, String rawName) {
        if (sym instanceof ExecutableElement ms) {
            // Calls into an anonymous class's own synthetic members (e.g.
            // its generated <init> calling super()) have no stable identity.
            Element directOwner = ms.getEnclosingElement();
            if (directOwner instanceof TypeElement ocs && ocs.getQualifiedName().length() == 0) {
                return;
            }
            String parent = SymbolNaming.ownerFqn(directOwner);
            String name = ms.getSimpleName().toString();
            // In targeted mode an error-symbol parent is not a resolvable
            // project method (phase-04 task-15); the full scan keeps its
            // exact behaviour, so this guard is filtered-only.
            if (parent != null && (!filtered || !parent.contains("<error>"))
                    && (name.equals("<init>") || name.indexOf('<') < 0) && !name.equals("<error>")) {
                String key = parent + "." + name + "(" + String.join(",", SymbolNaming.paramStrings(ms)) + ")";
                String id = funcID.get(key);
                if (id != null) {
                    writer.emitEdge("calls", mtd, id);
                    return;
                }
                // Targeted scan: the method belongs to an unchanged
                // (non-emitted) project package — emit against its
                // canonical FQN so the cached node resolves the edge.
                String canon = filtered ? surfaceFuncFqn.get(key) : null;
                if (canon != null) {
                    writer.emitEdge("calls", mtd, canon);
                    return;
                }
                String fqn = name.equals("<init>") ? parent + ".<init>" : parent + "." + name;
                writer.unresolved(fqn, UnresolvedCategory.categoryOf(parent));
                writer.emitUnresolvedCall(mtd, fqn);
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
                writer.emitEdge("calls", mtd, ctor);
                return;
            }
            String structFqn = projectStructBySimple().get(rawName);
            if (structFqn != null) {
                String fqn = structFqn + ".<init>";
                writer.unresolved(fqn, UnresolvedCategory.categoryOf(structFqn));
                writer.emitUnresolvedCall(mtd, fqn);
                return;
            }
        }
        if (recvFqn != null && structID.containsKey(recvFqn)) {
            writer.emitEdge("uses", mtd, structID.get(recvFqn));
        } else if (recvFqn != null && filtered && surfaceStructs.contains(recvFqn)) {
            writer.emitEdge("uses", mtd, recvFqn);
        } else {
            String target = rawName == null ? "?" : rawName;
            if (filtered && target.contains("<error>")) return;
            writer.unresolved(target, "unknown");
            writer.emitUnresolvedCall(mtd, target);
        }
    }

    /**
     * Records a type use: a project struct becomes a resolved `uses` edge,
     * an unresolvable type an `unresolved_use`. Resolved external types
     * are dropped (ubiquitous, low-signal).
     */
    void recordUse(String fromId, Tree type) {
        String tfqn = SymbolNaming.typeFqn(type, trees, getCurrentPath());
        if (tfqn != null) {
            if (structID.containsKey(tfqn)) {
                writer.emitEdge("uses", fromId, structID.get(tfqn));
            } else if (filtered && surfaceStructs.contains(tfqn)) {
                writer.emitEdge("uses", fromId, tfqn);
            }
            return;
        }
        String raw = SymbolNaming.typeRawName(type);
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
        writer.unresolved(raw, "unknown");
        writer.emitEdge("unresolved_use", fromId, raw);
    }

    /** Static type of the receiver expression of a method select, if any. */
    TypeMirror receiverType(ExpressionTree sel) {
        if (!(sel instanceof MemberSelectTree ms)) return null;
        ExpressionTree expr = ms.getExpression();
        return trees.getTypeMirror(new TreePath(getCurrentPath(), expr));
    }

    static String typeFqnOf(TypeMirror t) {
        if (t == null) return null;
        if (t instanceof DeclaredType dt && dt.asElement() instanceof TypeElement te) {
            String qn = te.getQualifiedName().toString();
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

    /** True when the declaration tree carries a doc comment. */
    boolean hasDocComment(Tree t) {
        if (docTrees == null) return false;
        try {
            return docTrees.getDocCommentTree(new TreePath(getCurrentPath(), t)) != null;
        } catch (Throwable ex) {
            return false;
        }
    }

    /** 0-based span of a class declaration, including its doc comment. */
    int[] classSpan(Tree t) {
        CompilationUnitTree cu = getCurrentPath().getCompilationUnit();
        int treeStart = (int) sourcePos.getStartPosition(cu, t);
        int start = treeStart;
        if (hasDocComment(t)) {
            for (int p = start; p >= 2; p--) {
                if (sourceText.charAt(p - 1) == '*' && sourceText.charAt(p - 2) == '/') {
                    start = p - 2;
                    break;
                }
            }
        }
        int bracePos = sourceText.indexOf('{', treeStart);
        int end = (int) sourcePos.getEndPosition(cu, t);
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
    int[] methodSpan(Tree t) {
        CompilationUnitTree cu = getCurrentPath().getCompilationUnit();
        int treeStart = (int) sourcePos.getStartPosition(cu, t);
        int start = treeStart;
        if (hasDocComment(t)) {
            for (int p = start; p >= 2; p--) {
                if (sourceText.charAt(p - 1) == '*' && sourceText.charAt(p - 2) == '/') {
                    start = p - 2;
                    break;
                }
            }
        }
        int end = (int) sourcePos.getEndPosition(cu, t);
        if (end < 0 && t instanceof MethodTree md && md.getBody() != null) {
            end = (int) sourcePos.getEndPosition(cu, md.getBody());
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
