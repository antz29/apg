package main

import (
	"go/ast"
	"go/types"
)

// emitStructUses emits `uses` edges for embedded struct fields and embedded
// interfaces (struct -> struct).
func emitStructUses(d fileDecl, ti *types.Info, modSet map[string]bool) {
	ts, ok := d.astNode.(*ast.TypeSpec)
	if !ok {
		return
	}
	emitUse := func(typ ast.Expr) {
		if tv, ok := ti.Types[typ]; ok && tv.Type != nil {
			if fqn := typeFQN(tv.Type, modSet); fqn != "" {
				if id, ok := structID[fqn]; ok {
					enc.Encode(edgeMsg{Type: "uses", From: d.id, To: edgeEndpoint(fqn, id)})
				}
			}
		}
	}
	if st, ok := ts.Type.(*ast.StructType); ok {
		for _, field := range st.Fields.List {
			if len(field.Names) == 0 {
				emitUse(field.Type)
			}
		}
	}
	if iface, ok := ts.Type.(*ast.InterfaceType); ok {
		for _, m := range iface.Methods.List {
			if len(m.Names) == 0 {
				emitUse(m.Type)
			}
		}
	}
}

// emitBodyEdges walks a function body, emitting call/use/unresolved edges with
// the enclosing function's id as the source.
func emitBodyEdges(body *ast.BlockStmt, sourceID string, ti *types.Info, modSet map[string]bool) {
	ast.Inspect(body, func(n ast.Node) bool {
		switch node := n.(type) {
		case *ast.CallExpr:
			cls := classifyCall(node, ti, modSet)
			switch cls.kind {
			case "call":
				if id, ok := funcID[cls.target]; ok {
					enc.Encode(edgeMsg{Type: "calls", From: sourceID, To: edgeEndpoint(cls.target, id)})
				}
			case "u_call":
				emitUnresolved(cls.target, cls.category)
				enc.Encode(edgeMsg{Type: "unresolved_call", From: sourceID, To: cls.target, TargetType: cls.targetType})
			case "use":
				if id, ok := structID[cls.target]; ok {
					enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(cls.target, id)})
				}
			case "u_use":
				emitUnresolved(cls.target, cls.category)
				enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: cls.target})
			}
			return true
		case *ast.CompositeLit:
			if node.Type != nil {
				if tv, ok := ti.Types[node.Type]; ok && tv.Type != nil {
					if fqn := typeFQN(tv.Type, modSet); fqn != "" {
						if id, ok := structID[fqn]; ok {
							enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(fqn, id)})
						}
					}
				} else {
					emitUnresolved(exprString(node.Type), "unknown")
					enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: exprString(node.Type)})
				}
			}
		case *ast.TypeAssertExpr:
			if node.Type != nil {
				if tv, ok := ti.Types[node.Type]; ok && tv.Type != nil {
					if fqn := typeFQN(tv.Type, modSet); fqn != "" {
						if id, ok := structID[fqn]; ok {
							enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(fqn, id)})
						}
					}
				} else {
					emitUnresolved(exprString(node.Type), "unknown")
					enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: exprString(node.Type)})
				}
			}
		case *ast.DeclStmt:
			gd, ok := node.Decl.(*ast.GenDecl)
			if !ok {
				return true
			}
			for _, spec := range gd.Specs {
				vs, ok := spec.(*ast.ValueSpec)
				if !ok {
					continue
				}
				if vs.Type != nil {
					if tv, ok := ti.Types[vs.Type]; ok && tv.Type != nil {
						if fqn := typeFQN(tv.Type, modSet); fqn != "" {
							if id, ok := structID[fqn]; ok {
								enc.Encode(edgeMsg{Type: "uses", From: sourceID, To: edgeEndpoint(fqn, id)})
							}
						}
					} else {
						emitUnresolved(exprString(vs.Type), "unknown")
						enc.Encode(edgeMsg{Type: "unresolved_use", From: sourceID, To: exprString(vs.Type)})
					}
				}
			}
		}
		return true
	})
}

// emitUnresolved emits the unresolved node record for fqn on first encounter
// (records are deduplicated by fqn; the first category wins).
func emitUnresolved(fqn, category string) {
	if fqn == "" || unresolvedSeen[fqn] {
		return
	}
	unresolvedSeen[fqn] = true
	enc.Encode(unresolvedMsg{Type: "unresolved", Fqn: fqn, Category: category})
}
