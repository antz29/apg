// apg unified JS/TS scanner frontend — AST walk and edge emission.
//
// Plain ESM sidecar (the `identity.mjs` precedent). Pass 2: walk each emitted
// file's syntax tree, recompute the current declared-function/struct context
// and emit the Calls/Uses/Unresolved edges. Resolution is exact (the injected
// `ctx.checker`); an unresolvable target becomes an UnresolvedTarget edge,
// never a fabricated FQN.

import ts from "typescript";
import { ctx, idByFqn, ctorIdByParent } from "./state.mjs";
import { primaryDecl } from "./declarations.mjs";
import { emitEdge, emitUnresolved } from "./emit.mjs";
import {
  idForDecl,
  structFqnOfDecl,
  categoryOfDecl,
  unwrapAlias,
  nameLocation,
  calleeText,
  isCallableDecl,
  typeNameLocation,
  isDeclName,
  isTypeRefNode,
} from "./resolve.mjs";

export function handleCall(node, sf, cur) {
  const loc = nameLocation(node.expression);
  if (!loc) {
    emitUnresolved(calleeText(node.expression, sf), "func-value");
    emitEdge("unresolved_call", cur, calleeText(node.expression, sf));
    return;
  }
  const sym = ctx.checker.getSymbolAtLocation(loc);
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const tid = decl ? idForDecl(decl) : "";
  if (tid) {
    // A class invoked without `new` (sloppy-mode constructor call) is a use of
    // the class, not a call to a Function node (calls edges must target
    // Functions).
    if (isCallableDecl(decl)) emitEdge("calls", cur, tid);
    else emitEdge("uses", cur, tid);
    return;
  }
  if (resolved) {
    const target = calleeText(node.expression, sf);
    if (!target) return;
    emitUnresolved(target, categoryOfDecl(decl));
    emitEdge("unresolved_call", cur, target);
    return;
  }
  emitUnresolved(calleeText(node.expression, sf), "unknown");
  emitEdge("unresolved_call", cur, calleeText(node.expression, sf));
}

export function handleNew(node, sf, cur) {
  const loc = nameLocation(node.expression);
  const sym = loc ? ctx.checker.getSymbolAtLocation(loc) : null;
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const classFqn = decl ? structFqnOfDecl(decl) : "";
  if (classFqn) {
    // A class with an explicit constructor is a call to it; otherwise the new
    // is a type instantiation → uses edge to the class.
    const ctorId = ctorIdByParent.get(classFqn);
    if (ctorId) emitEdge("calls", cur, ctorId);
    else {
      const id = idByFqn.get(classFqn);
      if (id) emitEdge("uses", cur, id);
    }
    return;
  }
  if (resolved) {
    const target = calleeText(node.expression, sf);
    if (!target) return;
    emitUnresolved(target, categoryOfDecl(decl));
    emitEdge("unresolved_use", cur, target);
    return;
  }
  emitUnresolved(calleeText(node.expression, sf), "unknown");
  emitEdge("unresolved_use", cur, calleeText(node.expression, sf));
}

export function handleType(node, sf, cur) {
  const loc = typeNameLocation(node);
  if (!loc) return;
  const sym = ctx.checker.getSymbolAtLocation(loc);
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const tid = decl ? idForDecl(decl) : "";
  if (tid) {
    emitEdge("uses", cur, tid);
    return;
  }
  if (resolved && decl) {
    const target = ctx.checker.symbolToString(resolved) || loc.getText(sf).trim();
    if (!target) return;
    emitUnresolved(target, categoryOfDecl(decl));
    emitEdge("unresolved_use", cur, target);
    return;
  }
  const t = loc.getText(sf).trim();
  if (!t) return;
  emitUnresolved(t, "unknown");
  emitEdge("unresolved_use", cur, t);
}

export function handleJsx(node, sf, cur) {
  // Component references in TSX (`<Button/>`, `<ui.Button/>`) are uses of the
  // component. Intrinsic elements (`<div/>`) resolve to the JSX namespace and
  // are skipped (no symbol at the location).
  const name = node.tagName;
  const loc = ts.isIdentifier(name) || ts.isPropertyAccessExpression(name) ? name : null;
  if (!loc) return;
  const sym = ctx.checker.getSymbolAtLocation(loc);
  const resolved = sym ? unwrapAlias(sym) : null;
  const decl = resolved ? primaryDecl(resolved) : null;
  const tid = decl ? idForDecl(decl) : "";
  if (tid) emitEdge("uses", cur, tid);
}

export function walkNode(node, sf, cur) {
  if (!node) return;

  // Recompute cur when entering a declared function or struct-like node.
  if (
    ts.isFunctionDeclaration(node) ||
    ts.isMethodDeclaration(node) ||
    ts.isMethodSignature(node) ||
    ts.isConstructorDeclaration(node) ||
    ts.isGetAccessor(node) ||
    ts.isSetAccessor(node)
  ) {
    const newCur = idForDecl(node) || cur;
    ts.forEachChild(node, (c) => walkNode(c, sf, newCur));
    return;
  }
  if (
    ts.isClassDeclaration(node) ||
    ts.isInterfaceDeclaration(node) ||
    ts.isEnumDeclaration(node) ||
    ts.isTypeAliasDeclaration(node) ||
    ts.isModuleDeclaration(node)
  ) {
    const newCur = idForDecl(node) || cur;
    // extends/implements/type-param constraints attribute to the type itself.
    if (newCur && (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node))) {
      if (node.heritageClauses) {
        for (const hc of node.heritageClauses) {
          for (const t of hc.types) handleType(t, sf, newCur);
        }
      }
      if (node.typeParameters) {
        for (const tp of node.typeParameters) {
          if (tp.constraint) handleType(tp.constraint, sf, newCur);
        }
      }
    }
    ts.forEachChild(node, (c) => walkNode(c, sf, newCur));
    return;
  }
  // Function-valued variable declarations: attribute edges in the initializer
  // to the declared function node.
  if (ts.isVariableDeclaration(node)) {
    const newCur = idForDecl(node) || cur;
    ts.forEachChild(node, (c) => walkNode(c, sf, newCur));
    return;
  }

  // Edge extraction for nodes directly in the current context.
  if (cur) {
    if (ts.isCallExpression(node)) handleCall(node, sf, cur);
    else if (ts.isNewExpression(node)) handleNew(node, sf, cur);
    else if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) handleJsx(node, sf, cur);
    else if (isTypeRefNode(node)) handleType(node, sf, cur);
    else if (ts.isIdentifier(node) && !isDeclName(node)) {
      // A class/enum/namespace used as a value (`let x = Foo`).
      const sym = ctx.checker.getSymbolAtLocation(node);
      const resolved = sym ? unwrapAlias(sym) : null;
      if (resolved && resolved.declarations) {
        const decl = resolved.declarations.find(
          (d) => ts.isClassDeclaration(d) || ts.isEnumDeclaration(d) || ts.isTypeAliasDeclaration(d)
        );
        const tid = decl ? idForDecl(decl) : "";
        if (tid) emitEdge("uses", cur, tid);
      }
    }
  }
  ts.forEachChild(node, (c) => walkNode(c, sf, cur));
}
