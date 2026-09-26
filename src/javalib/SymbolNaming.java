import com.sun.source.tree.*;
import com.sun.source.util.*;
import javax.lang.model.element.*;
import javax.lang.model.type.*;
import java.util.*;

class SymbolNaming {
    // ------------------------------------------------------------------
    // Java module identity (phase-04)
    //
    // Every fact that names a scope goes through one of these four
    // side-effect-free helpers: the module record (emitPkgHierarchy), the
    // File record's parent (visitCompilationUnit) and a class's scope parent
    // + in-stream identity (visitClass). The empty (default) package is the
    // degenerate case: it must still produce a STABLE, NON-EMPTY module
    // identity, or the module record is suppressed, the File is orphaned and
    // its top-level class renders with a leading dot.
    // ------------------------------------------------------------------

    /**
     * The module identity of the DEFAULT (unnamed) Java package. An empty
     * package still needs a reachable root module — a module record, a
     * non-empty File parent and a class parent with no leading dot — so it
     * maps to this single, dot-free segment. It cannot collide with a real
     * package or type: Java identifiers admit neither a space nor the
     * parentheses, so no declared name can ever equal it.
     */
    static final String DEFAULT_PACKAGE_IDENTITY = "(default)";

    /**
     * The module identity a source's declared package is emitted as: a
     * packaged input passes through unchanged, the empty (default) package
     * yields {@link #DEFAULT_PACKAGE_IDENTITY} so its sources still get a root
     * module. Pure: plain Strings only — no compilation unit, tree, file or
     * output is touched.
     */
    static String moduleIdentityFor(String pkg) {
        return (pkg == null || pkg.isEmpty()) ? DEFAULT_PACKAGE_IDENTITY : pkg;
    }

    /**
     * The parent module identity of a source File's record: the source's
     * module identity, never empty and never a leading-dot prefix. Pure.
     */
    static String fileParentFor(String pkg) {
        return moduleIdentityFor(pkg);
    }

    /**
     * The scope parent of a class declaration. A NESTED class keeps its
     * enclosing class as parent (`pkg.Outer`, or the bare `Outer` in the
     * default package); a TOP-LEVEL class's parent is its declaring package's
     * module identity — never empty, never a leading dot. Pure.
     */
    static String classParentFor(String pkg, String outer) {
        if (outer == null || outer.isEmpty()) return moduleIdentityFor(pkg);
        return (pkg == null || pkg.isEmpty()) ? outer : pkg + "." + outer;
    }

    /**
     * The class's in-stream declaration identity: `pkg.cls`, or the bare `cls`
     * in the default package (no leading dot). This is the scanner's
     * self-consistent key — shared with the member function parents and the
     * unchanged-package declaration surface — so it deliberately keeps the
     * bare class name for the default package even though the emitted scope
     * parent is the default module identity. Pure.
     */
    static String classFqnFor(String pkg, String cls) {
        return (pkg == null || pkg.isEmpty()) ? cls : pkg + "." + cls;
    }

    /**
     * Returns the fully-qualified owning class (package.Outer.Inner) for a
     * symbol, walking up past synthetic anonymous/local classes ($-suffixed)
     * to the nearest named type.
     */
    static String ownerFqn(Element owner) {
        while (owner instanceof TypeElement te) {
            String qn = te.getQualifiedName().toString();
            // Anonymous classes have an empty qualified name; walk up past them
            // (and past $-named synthetic classes) to the nearest named type.
            if (!qn.isEmpty() && qn.indexOf('$') < 0) return qn;
            owner = te.getEnclosingElement();
        }
        return null;
    }

    /**
     * Erased, package-qualified parameter types of a method symbol, in
     * declaration order (SPEC §2.3). The same rendering is used for method
     * declarations and call targets, so overloads resolve exactly.
     */
    static List<String> paramStrings(ExecutableElement ms) {
        List<String> out = new ArrayList<>();
        if (ms == null) return out;
        try {
            for (VariableElement p : ms.getParameters()) {
                out.add(typeString(p.asType()));
            }
        } catch (Throwable t) {
            out.clear();
        }
        return out;
    }

    /** Renders a type mirror: qualified for declared types, recursed for arrays. */
    static String typeString(TypeMirror t) {
        if (t == null) return "";
        if (t instanceof ArrayType at) {
            return typeString(at.getComponentType()) + "[]";
        }
        if (t instanceof DeclaredType dt && dt.asElement() instanceof TypeElement te) {
            return te.getQualifiedName().toString();
        }
        return t.toString();
    }

    /**
     * Resolves the fully-qualified type name from an attributed type tree.
     * Arrays recurse to their element type; parameterized types use the raw
     * type element. Null if the tree has no usable type.
     */
    static String typeFqn(Tree typeTree, Trees trees, TreePath path) {
        if (typeTree == null) return null;
        if (typeTree instanceof ArrayTypeTree at) return typeFqn(at.getType(), trees, path);
        TypeMirror t = trees.getTypeMirror(new TreePath(path, typeTree));
        if (t instanceof DeclaredType dt && dt.asElement() instanceof TypeElement te) {
            String qn = te.getQualifiedName().toString();
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

    /** Method/constructor element attributed onto a call/method-ref/new-class expression, or null. */
    static Element symOf(Trees trees, TreePath path, Tree t) {
        if (t == null) return null;
        return trees.getElement(new TreePath(path, t));
    }

    /** Method element attributed onto a method declaration, or null. */
    static ExecutableElement symOfDecl(Trees trees, TreePath path, MethodTree mt) {
        if (mt == null) return null;
        Element e = trees.getElement(new TreePath(path, mt));
        return e instanceof ExecutableElement ee ? ee : null;
    }

    /** Simple class name of an FQN (its last `.`/`$` segment). */
    static String simpleName(String fqn) {
        if (fqn == null) return "";
        int d = Math.max(fqn.lastIndexOf('.'), fqn.lastIndexOf('$'));
        return d >= 0 ? fqn.substring(d + 1) : fqn;
    }

    static String groupKey(String funcKey) {
        int p = funcKey.indexOf('(');
        String prefix = p >= 0 ? funcKey.substring(0, p) : funcKey;
        int d = prefix.lastIndexOf('.');
        return d >= 0 ? prefix.substring(0, d) + "\u0000" + prefix.substring(d + 1) : prefix;
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
}
