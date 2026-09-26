package main

import (
	"go/ast"
	"go/types"
	"strings"

	"golang.org/x/tools/go/packages"
)

// callClass is the result of classifying a call expression.
type callClass struct {
	kind       string // "call", "u_call", "use", "u_use"
	target     string // edge target FQN (or raw name)
	category   string // UnresolvedTarget.category (u_call / u_use only)
	targetType string // function type of a func-value call (u_call only)
}

// classifyCall resolves a call expression to an edge. Order matters:
// conversions are not calls, then project functions, then builtins, interface
// methods, function-valued variables, and finally external functions.
func classifyCall(call *ast.CallExpr, ti *types.Info, modSet map[string]bool) callClass {
	// Type conversion ([]byte(x), protoimpl.Pointer(x), (*T)(nil), T(x)) —
	// not a function call. Route to a type-use edge instead.
	if tv, ok := ti.Types[call.Fun]; ok && tv.IsType() {
		return classifyConversion(call, tv.Type, ti, modSet)
	}
	// Immediately-invoked function literal: anonymous, not a named function.
	if isIIFE(call.Fun) {
		tt := ""
		if tv, ok := ti.Types[call.Fun]; ok && tv.Type != nil {
			tt = sigString(tv.Type)
		}
		return callClass{"u_call", "func", "func-value", tt}
	}

	obj, isMethodVal := callObject(call, ti)
	if obj == nil {
		return callClass{"u_call", callRawName(call), "unknown", ""}
	}

	switch o := obj.(type) {
	case *types.Func:
		if o.Pkg() != nil && inProjectModules(o.Pkg().Path(), modSet) {
			if fqn := funcFQNAny(o); fqn != "" {
				return callClass{"call", fqn, "", ""}
			}
			// Project method with no resolvable FQN (e.g. method declared on
			// an anonymous interface type) — fall through to unresolved.
		}
		fqn := funcFQNAny(o)
		if fqn == "" {
			if isMethodVal {
				return callClass{"u_call", callRawName(call), "interface-method", ""}
			}
			return callClass{"u_call", callRawName(call), "unknown", ""}
		}
		if o.Pkg() == nil {
			// Universe-scope interface method (e.g. error.Error).
			return callClass{"u_call", fqn, "interface-method", ""}
		}
		return callClass{"u_call", fqn, stdOrExternal(o.Pkg().Path()), ""}
	case *types.Builtin:
		return callClass{"u_call", o.Name(), "builtin", ""}
	case *types.Var:
		if !isFuncType(o.Type()) {
			return callClass{"u_call", callRawName(call), "unknown", ""}
		}
		// Function-valued variable. Package-level vars have a resolvable
		// identity; locals are recorded by bare name.
		if o.Pkg() != nil && o.Parent() == o.Pkg().Scope() {
			return callClass{"u_call", o.Pkg().Path() + "." + o.Name(), "func-value", sigString(o.Type())}
		}
		return callClass{"u_call", o.Name(), "func-value", sigString(o.Type())}
	}
	return callClass{"u_call", callRawName(call), "unknown", ""}
}

// classifyConversion routes a type-conversion call expression to a type-use
// edge: a project type becomes a resolved use, anything else an unresolved use.
func classifyConversion(call *ast.CallExpr, t types.Type, ti *types.Info, modSet map[string]bool) callClass {
	// A named/aliased target gives an exact type identity even when the alias
	// resolves to a builtin underlying type (e.g. protoimpl.Pointer = unsafe.Pointer).
	if obj, _ := callObject(call, ti); obj != nil {
		if tn, ok := obj.(*types.TypeName); ok {
			if tn.Pkg() == nil {
				return callClass{"u_use", exprString(call.Fun), "builtin", ""}
			}
			fqn := tn.Pkg().Path() + "." + tn.Name()
			if inProjectModules(tn.Pkg().Path(), modSet) {
				return callClass{"use", fqn, "", ""}
			}
			return callClass{"u_use", fqn, stdOrExternal(tn.Pkg().Path()), ""}
		}
	}
	// Compound types ([]byte, *T, map[...]...) resolved through the type.
	if fqn := typeFQN(t, modSet); fqn != "" {
		return callClass{"use", fqn, "", ""}
	}
	if fqn := typeFQNAny(t); fqn != "" {
		return callClass{"u_use", fqn, typeCategory(t), ""}
	}
	return callClass{"u_use", exprString(call.Fun), typeCategory(t), ""}
}

// callObject returns the resolved object behind a call's Fun expression,
// unwrapping generic instantiations. isMethodVal reports whether the object
// came from a method-value selection.
func callObject(call *ast.CallExpr, ti *types.Info) (types.Object, bool) {
	switch fun := call.Fun.(type) {
	case *ast.Ident:
		return ti.Uses[fun], false
	case *ast.SelectorExpr:
		if sel, ok := ti.Selections[fun]; ok && sel.Kind() == types.MethodVal {
			return sel.Obj(), true
		}
		return ti.Uses[fun.Sel], false
	case *ast.IndexExpr:
		return callObject(&ast.CallExpr{Fun: fun.X}, ti)
	case *ast.IndexListExpr:
		return callObject(&ast.CallExpr{Fun: fun.X}, ti)
	}
	return nil, false
}

// isIIFE reports whether fun is an immediately-invoked function literal,
// possibly parenthesized.
func isIIFE(fun ast.Expr) bool {
	switch f := fun.(type) {
	case *ast.FuncLit:
		return true
	case *ast.ParenExpr:
		_, ok := f.X.(*ast.FuncLit)
		return ok
	}
	return false
}

func isFuncType(t types.Type) bool {
	if t == nil {
		return false
	}
	_, ok := t.Underlying().(*types.Signature)
	return ok
}

// sigString renders a function type as a compact, package-qualified string.
func sigString(t types.Type) string {
	if t == nil {
		return ""
	}
	return types.TypeString(t, func(p *types.Package) string {
		if p == nil {
			return ""
		}
		return p.Path()
	})
}

// paramStrings renders a signature's parameter types (excluding the receiver),
// package-qualified, in declaration order (SPEC §2.3).
func paramStrings(sig *types.Signature) []string {
	var out []string
	params := sig.Params()
	for i := 0; i < params.Len(); i++ {
		out = append(out, types.TypeString(params.At(i).Type(), func(p *types.Package) string {
			if p == nil {
				return ""
			}
			return p.Path()
		}))
	}
	if out == nil {
		out = []string{}
	}
	return out
}

// funcParams returns the call-signature parameter types of a declared function.
func funcParams(d *ast.FuncDecl, p *packages.Package) []string {
	obj, ok := p.TypesInfo.Defs[d.Name]
	if !ok {
		return []string{}
	}
	fn, ok := obj.(*types.Func)
	if !ok {
		return []string{}
	}
	return paramStrings(fn.Type().(*types.Signature))
}

// interfaceMethodParams returns the parameter types of an interface method
// declaration.
func interfaceMethodParams(m *ast.Field, ti *types.Info) []string {
	if len(m.Names) == 0 {
		return []string{}
	}
	if obj, ok := ti.Defs[m.Names[0]]; ok {
		if fn, ok := obj.(*types.Func); ok {
			return paramStrings(fn.Type().(*types.Signature))
		}
	}
	return []string{}
}

// stdOrExternal classifies a package path as stdlib (first segment has no dot)
// or external (first segment is a domain, e.g. github.com, google.golang.org).
func stdOrExternal(pkgPath string) string {
	seg := pkgPath
	if i := strings.IndexByte(seg, '/'); i >= 0 {
		seg = seg[:i]
	}
	if strings.Contains(seg, ".") {
		return "external"
	}
	return "stdlib"
}

// typeCategory classifies a type for an unresolved-use target.
func typeCategory(t types.Type) string {
	switch tt := t.(type) {
	case *types.Named:
		if tt.Obj().Pkg() == nil {
			return "builtin"
		}
		return stdOrExternal(tt.Obj().Pkg().Path())
	case *types.Pointer:
		return typeCategory(tt.Elem())
	case *types.Slice:
		return typeCategory(tt.Elem())
	case *types.Array:
		return typeCategory(tt.Elem())
	case *types.Map:
		return typeCategory(tt.Elem())
	default:
		return "builtin"
	}
}

func callRawName(call *ast.CallExpr) string {
	var name string
	switch fun := call.Fun.(type) {
	case *ast.Ident:
		name = fun.Name
	case *ast.SelectorExpr:
		name = fun.Sel.Name
	case *ast.IndexExpr:
		name = callRawName(&ast.CallExpr{Fun: fun.X})
	case *ast.FuncLit:
		// Immediately-invoked function literal: not a call to a named
		// function. Record a short, stable name instead of dumping the
		// whole function body.
		name = "func"
	case *ast.ParenExpr:
		if _, ok := fun.X.(*ast.FuncLit); ok {
			name = "func"
		} else {
			name = exprString(call.Fun)
		}
	default:
		name = exprString(call.Fun)
	}
	return name
}
