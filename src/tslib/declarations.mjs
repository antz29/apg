// apg unified JS/TS scanner frontend — declaration extraction.
//
// Plain ESM sidecar (the `identity.mjs` precedent). Collects the structs and
// functions a source file declares into the shared accumulators (state.mjs).
// Structs keyed by FQN (parent.name); functions keyed by `parent.name(params)`
// so overloads stay distinct (the ingestor renders the FQN from parent/name/
// params). Ids are assigned later by the orchestrator, after sorting by
// (path, start) for determinism.

import ts from "typescript";
import {
  structs,
  funcs,
  structOrder,
  funcOrder,
  declIdByKey,
  structFqnByKey,
  structByParentName,
} from "./state.mjs";
import { relPrefix } from "./discovery.mjs";

export function lineOf(sf, pos) {
  if (pos < 0) return 1;
  return sf.getLineAndCharacterOfPosition(pos).line + 1;
}
export function lineEndOf(sf, end) {
  return end <= 0 ? 1 : lineOf(sf, end - 1);
}

// 0-based start of a declaration including a leading JSDoc comment (matches
// the Java frontend, which includes doc comments in spans).
export function docStart(node, sf) {
  let start = node.getStart(sf);
  const docs = ts.getJSDocCommentsAndTags(node);
  if (docs && docs.length > 0) {
    const dstart = docs[0].getStart(sf);
    if (dstart < start) start = dstart;
  }
  return start;
}

export function keyOf(sf, node) {
  return sf.fileName + "@" + docStart(node, sf);
}

export function registerStruct(parent, name, node, sf) {
  const fqn = parent + "." + name;
  const dup = parent + "\0" + name;
  if (structByParentName.has(dup)) return ""; // declaration merging — first wins
  structByParentName.set(dup, fqn);
  const start = docStart(node, sf);
  const end = node.end;
  const d = {
    id: "",
    parent,
    name,
    path: sf.fileName,
    start,
    end,
    sl: lineOf(sf, start),
    el: lineEndOf(sf, end),
    decl: node,
  };
  structs.set(fqn, d);
  structOrder.push(fqn);
  declIdByKey.set(keyOf(sf, node), ""); // id assigned later
  structFqnByKey.set(keyOf(sf, node), fqn);
  return fqn;
}

export function registerFunction(parent, name, params, node, sf) {
  const key = parent + "." + name + "(" + params.join(",") + ")";
  if (funcs.has(key)) return ""; // exact duplicate (getter/setter are distinct)
  const start = docStart(node, sf);
  const end = node.end;
  const d = {
    id: "",
    parent,
    name,
    params,
    path: sf.fileName,
    file: sf.fileName,
    start,
    end,
    sl: lineOf(sf, start),
    el: lineEndOf(sf, end),
    decl: node,
  };
  funcs.set(key, d);
  funcOrder.push(key);
  declIdByKey.set(keyOf(sf, node), ""); // id assigned later
  return key;
}

export function paramTypes(node, sf) {
  const ps = [];
  if (!node.parameters) return ps;
  for (const p of node.parameters) {
    ps.push(p.type ? p.type.getText(sf).trim() : "any");
  }
  return ps;
}

// The declaration of a symbol that becomes a node: for an overloaded function
// the implementation signature (last, has a body), else the first declaration.
export function primaryDecl(sym) {
  if (!sym || !sym.declarations || sym.declarations.length === 0) return null;
  for (let i = sym.declarations.length - 1; i >= 0; i--) {
    const d = sym.declarations[i];
    if (
      ts.isMethodDeclaration(d) ||
      ts.isConstructorDeclaration(d) ||
      ts.isFunctionDeclaration(d) ||
      ts.isGetAccessor(d) ||
      ts.isSetAccessor(d)
    ) {
      if (d.body) return d;
    }
  }
  return sym.declarations[0];
}

// Collects declarations in one source file. `container` is the current
// enclosing-scope parent FQN (starts at the file's `pkg.relpath` prefix).
export function collectFile(sf, pkgDir, modFqn) {
  const fileParent = modFqn + "." + relPrefix(pkgDir, sf.fileName);

  // Function-valued variables (`const foo = () => {}`) are declared functions.
  const registerVariableFunctions = (node, parent) => {
    const decls = node.declarationList ? node.declarationList.declarations : [];
    for (const decl of decls) {
      if (!ts.isVariableDeclaration(decl)) continue;
      const init = decl.initializer;
      let fn = null;
      if (init && (ts.isArrowFunction(init) || ts.isFunctionExpression(init))) fn = init;
      else if (init && ts.isParenthesizedExpression(init) && (ts.isArrowFunction(init.expression) || ts.isFunctionExpression(init.expression))) fn = init.expression;
      if (fn && ts.isIdentifier(decl.name)) {
        registerFunction(parent, decl.name.text, paramTypes(fn, sf), decl, sf);
      }
    }
  };

  const walk = (node, parent) => {
    if (!node) return;

    if (ts.isVariableStatement(node)) {
      registerVariableFunctions(node, parent);
      return;
    }

    if (ts.isClassDeclaration(node) || ts.isInterfaceDeclaration(node)) {
      const name = node.name ? node.name.text : null;
      if (name) {
        const fqn = registerStruct(parent, name, node, sf);
        if (fqn) {
          for (const m of node.members) {
            if (ts.isConstructorDeclaration(m)) {
              registerFunction(fqn, "constructor", paramTypes(m, sf), m, sf);
            } else if (ts.isMethodDeclaration(m) || ts.isMethodSignature(m) || ts.isGetAccessor(m) || ts.isSetAccessor(m)) {
              const mname = m.name && ts.isIdentifier(m.name) ? m.name.text : (m.name && ts.isStringLiteral(m.name) ? m.name.text : null);
              if (!mname) continue;
              registerFunction(fqn, mname, paramTypes(m, sf), m, sf);
            } else if (ts.isPropertyDeclaration(m) && m.name && ts.isIdentifier(m.name)) {
              const init = m.initializer;
              let fn = null;
              if (init && (ts.isArrowFunction(init) || ts.isFunctionExpression(init))) fn = init;
              else if (init && ts.isParenthesizedExpression(init) && (ts.isArrowFunction(init.expression) || ts.isFunctionExpression(init.expression))) fn = init.expression;
              if (fn) registerFunction(fqn, m.name.text, paramTypes(fn, sf), m, sf);
            }
          }
        }
      }
      return;
    }

    if (ts.isEnumDeclaration(node) || ts.isTypeAliasDeclaration(node)) {
      if (node.name) registerStruct(parent, node.name.text, node, sf);
      return;
    }

    if (ts.isModuleDeclaration(node)) {
      const name = node.name;
      if (!name || !ts.isIdentifier(name)) return; // ambient external modules are typings
      const fqn = registerStruct(parent, name.text, node, sf);
      const body = node.body;
      if (body && ts.isModuleBlock(body)) {
        for (const s of body.statements) walk(s, fqn);
      }
      return;
    }

    if (ts.isFunctionDeclaration(node)) {
      if (node.name && node.body) {
        registerFunction(parent, node.name.text, paramTypes(node, sf), node, sf);
      }
      return;
    }
  };

  for (const stmt of sf.statements) walk(stmt, fileParent);
}
