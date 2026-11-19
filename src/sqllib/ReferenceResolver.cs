using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace Apg.SqlFrontend
{
    // Pure reference resolution over SchemaEmitter's declaration index: Collect gathers one unit's edges, Flush writes them.
    public sealed class ReferenceResolver
    {
        // The finer declaration kind Declaration.IsFunction cannot tell; Emit (task 5.2) supplies it per emitted id.
        public enum Kind { Table, View, Sequence, Synonym, Type, TableType, Procedure, Function, Trigger }

        static readonly Kind[] ObjectKinds = { Kind.Table, Kind.View, Kind.Sequence, Kind.Synonym };
        static readonly Kind[] TypeKinds = { Kind.Type, Kind.TableType };
        static readonly Kind[] TriggerTargets = { Kind.Table, Kind.View };
        static readonly Kind[] Procedures = { Kind.Procedure, Kind.Synonym };
        static readonly Kind[] Functions = { Kind.Function, Kind.Synonym };
        static readonly Kind[] SynonymObjects = { Kind.Table, Kind.View, Kind.Sequence };
        static readonly Kind[] SynonymCallees = { Kind.Procedure, Kind.Function };
        static readonly IReadOnlyDictionary<ColumnDefinition, string> NoColumnIds = new Dictionary<ColumnDefinition, string>();

        static readonly string[] SystemTypes = { "sysname", "geography", "geometry", "hierarchyid" };

        readonly Dictionary<SqlProjectReader.Scope, Dictionary<string, SchemaEmitter.Declaration>> index;
        readonly IReadOnlyDictionary<string, Kind> kinds;
        // uses, calls, unresolved_use and unresolved_call in unit-then-source order; a calls To may be a synonym id,
        // mapped at Flush; an unresolved To is the upper-cased name key into `unresolved`.
        readonly List<(string Type, string From, string To, string Kind)> edges = new List<(string, string, string, string)>();
        // (from, to) -> its position in edges: a repeat use only raises the kind (read < write < trigger).
        readonly Dictionary<(string, string), int> positions = new Dictionary<(string, string), int>();
        readonly HashSet<(string, string, string)> seen = new HashSet<(string, string, string)>();
        // Synonym id -> its procedure or function target id, filled as synonyms are collected and read only at Flush,
        // so a call through a synonym collected after its caller still resolves.
        readonly Dictionary<string, string> synonymCallees = new Dictionary<string, string>();
        // Synonym id -> the unresolved name key of its absent target, so a call through it is an unresolved call.
        readonly Dictionary<string, string> synonymTargets = new Dictionary<string, string>();
        // Upper-cased name -> category and spelling of its earliest site across the whole scan: units are Collected in
        // ordinal repo-relative path then declaration order (unit = Collect sequence), and offsets order sites within one.
        readonly Dictionary<string, (string Category, string Name, int Unit, int Offset)> unresolved =
            new Dictionary<string, (string, string, int, int)>(StringComparer.Ordinal);
        int units;

        // index: Emit's per-scope Schema.Name (ordinal-ignore-case) -> emitted declaration; kindById: each declaration id's kind.
        public ReferenceResolver(Dictionary<SqlProjectReader.Scope, Dictionary<string, SchemaEmitter.Declaration>> index,
            IReadOnlyDictionary<string, Kind> kindById)
        {
            this.index = index; kinds = kindById;
        }

        // One emitted unit: views, procedures, functions and triggers yield object (and, but views, type) uses from unitId;
        // a table or table type yields its columns' type uses from the column ids; every other kind yields nothing here.
        public void Collect(SqlProjectReader.Scope scope, string path, string defaultSchema, TSqlStatement statement,
            string unitId, Kind unitKind, IReadOnlyDictionary<ColumnDefinition, string> columnIds)
        {
            units++;
            var unit = new Unit(this, scope, defaultSchema, unitId);
            switch (unitKind)
            {
                case Kind.Table:
                case Kind.TableType:
                    var defs = (statement as CreateTableStatement)?.Definition?.ColumnDefinitions
                        ?? (statement as CreateTypeTableStatement)?.Definition?.ColumnDefinitions;
                    if (defs == null) break;
                    foreach (var d in defs)
                        if ((columnIds ?? NoColumnIds).TryGetValue(d, out var columnId)) unit.Type(columnId, d.DataType);
                    break;
                case Kind.Synonym:
                    var target = (statement as CreateSynonymStatement)?.ForName; // the FOR <base object> name
                    var absent = unit.Add(target, SynonymObjects, "read");
                    if (absent != null) { synonymTargets[unitId] = absent; break; }
                    var callee = Resolve(scope, defaultSchema, target, SynonymCallees);
                    if (callee != null) synonymCallees[unitId] = callee.Id;
                    break;
                case Kind.View:
                case Kind.Procedure:
                case Kind.Function:
                case Kind.Trigger:
                    bool dml = statement is TriggerStatementBody t && t.TriggerObject.TriggerScope == TriggerScope.Normal;
                    if (dml) unit.Add(((TriggerStatementBody)statement).TriggerObject.Name, TriggerTargets, "trigger");
                    var locals = new HashSet<TSqlFragment>();
                    var writes = new HashSet<TSqlFragment>();
                    // Pass one marks locals and write targets; pass two collects in source order.
                    statement.Accept(new Walker(unit, dml, unitKind != Kind.View, false, locals, writes));
                    statement.Accept(new Walker(unit, dml, unitKind != Kind.View, true, locals, writes));
                    break;
            }
        }

        // Every unresolved record first, once per name, so each precedes any edge naming it; then the edges in order.
        public void Flush(TextWriter stdout)
        {
            void Write(object record) => stdout.WriteLine(JsonSerializer.Serialize(record));
            var named = new HashSet<string>(StringComparer.Ordinal);
            foreach (var e in edges)
                if (e.Type != "uses" && e.Type != "calls" && named.Add(e.To))
                    Write(new { type = "unresolved", fqn = unresolved[e.To].Name, category = unresolved[e.To].Category });
            var emitted = new HashSet<(string, string, string)>();
            foreach (var (type, from, raw, kind) in edges)
            {
                if (type == "uses") { Write(new { type, from, to = raw, properties = new { kind } }); continue; }
                string t = type, to = raw;
                if (type == "calls" && kinds.TryGetValue(raw, out var k) && k == Kind.Synonym && !synonymCallees.TryGetValue(raw, out to))
                {
                    if (!synonymTargets.TryGetValue(raw, out to)) continue;
                    t = "unresolved_call";
                }
                if (!emitted.Add((t, from, to))) continue;
                if (t == "calls") Write(new { type = t, from, to });
                else Write(new { type = t, from, to = unresolved[to].Name });
            }
        }

        static int Rank(string kind) => kind == "trigger" ? 2 : kind == "write" ? 1 : 0;

        // The declaration a one- or two-part name names in the unit's own scope, of any kind; three- and four-part never do.
        SchemaEmitter.Declaration Find(SqlProjectReader.Scope scope, string defaultSchema, SchemaObjectName name)
        {
            if (name?.BaseIdentifier == null || name.ServerIdentifier != null || name.DatabaseIdentifier != null) return null;
            return index.TryGetValue(scope, out var decls)
                && decls.TryGetValue((name.SchemaIdentifier?.Value ?? defaultSchema) + "." + name.BaseIdentifier.Value, out var d) ? d : null;
        }

        bool Is(SchemaEmitter.Declaration d, Kind[] wanted) => kinds.TryGetValue(d.Id, out var k) && wanted.Contains(k);

        // A one- or two-part name of a wanted kind in the unit's own scope.
        SchemaEmitter.Declaration Resolve(SqlProjectReader.Scope scope, string defaultSchema, SchemaObjectName name, Kind[] wanted)
        {
            var d = Find(scope, defaultSchema, name);
            return d != null && Is(d, wanted) ? d : null;
        }

        void Edge(string from, string to, string kind)
        {
            if (!positions.TryGetValue((from, to), out int i)) { positions[(from, to)] = edges.Count; edges.Add(("uses", from, to, kind)); }
            else if (Rank(kind) > Rank(edges[i].Kind)) edges[i] = ("uses", from, to, kind);
        }

        void Call(string from, string to)
        {
            if (seen.Add(("calls", from, to))) edges.Add(("calls", from, to, null));
        }

        // One unresolved edge per (unit, name); the name keeps the category and spelling of its earliest site. Returns the key.
        string Unresolved(string type, string from, string category, string name, TSqlFragment at)
        {
            string key = name.ToUpperInvariant();
            int offset = at?.StartOffset ?? 0;
            if (!unresolved.TryGetValue(key, out var first) || units < first.Unit || (units == first.Unit && offset < first.Offset))
                unresolved[key] = (category, name, units, offset);
            if (seen.Add((type, from, key))) edges.Add((type, from, key, null));
            return key;
        }

        static bool OnePart(SchemaObjectName n) => n.SchemaIdentifier == null && n.DatabaseIdentifier == null && n.ServerIdentifier == null;

        static bool IsSchema(string schema, string wanted) => string.Equals(schema, wanted, StringComparison.OrdinalIgnoreCase);

        // The written name parts, brackets stripped, joined by dots; a one-part name takes the file's default schema.
        static string Written(SchemaObjectName n, string defaultSchema)
        {
            if (OnePart(n)) return defaultSchema + "." + n.BaseIdentifier.Value;
            var parts = new[] { n.ServerIdentifier, n.DatabaseIdentifier, n.SchemaIdentifier, n.BaseIdentifier };
            return string.Join(".", parts.SkipWhile(p => p == null).Select(p => p?.Value ?? ""));
        }

        sealed class Unit
        {
            readonly ReferenceResolver r;
            readonly SqlProjectReader.Scope scope;
            readonly string schema;
            public readonly string Id;

            public Unit(ReferenceResolver resolver, SqlProjectReader.Scope s, string defaultSchema, string id)
            {
                r = resolver; scope = s; schema = defaultSchema; Id = id;
            }

            // An object reference: a uses edge when declared of a wanted kind, nothing when declared of another, else an
            // unresolved use — a one- or two-part sys or INFORMATION_SCHEMA name is a builtin catalog object. Returns its key.
            public string Add(SchemaObjectName name, Kind[] wanted, string kind)
            {
                if (name?.BaseIdentifier == null) return null;
                var d = r.Find(scope, schema, name);
                if (d != null) { if (r.Is(d, wanted)) r.Edge(Id, d.Id, kind); return null; }
                string s = name.SchemaIdentifier?.Value ?? schema;
                bool catalog = name.DatabaseIdentifier == null && name.ServerIdentifier == null
                    && (IsSchema(s, "sys") || IsSchema(s, "INFORMATION_SCHEMA"));
                return catalog ? r.Unresolved("unresolved_use", Id, "builtin", (s + "." + name.BaseIdentifier.Value).ToUpperInvariant(), name)
                    : r.Unresolved("unresolved_use", Id, "external", Written(name, schema), name);
            }

            // A schema-less or sys-qualified sysname, geography, geometry or hierarchyid is a system type: no record, no edge.
            public void Type(string from, DataTypeReference type)
            {
                if (type is not UserDataTypeReference u || u.Name?.BaseIdentifier == null) return;
                var n = u.Name;
                if (n.DatabaseIdentifier == null && n.ServerIdentifier == null && (n.SchemaIdentifier == null || IsSchema(n.SchemaIdentifier.Value, "sys"))
                    && SystemTypes.Any(t => IsSchema(n.BaseIdentifier.Value, t)))
                    return;
                var d = r.Find(scope, schema, n);
                if (d != null) { if (r.Is(d, TypeKinds)) r.Edge(from, d.Id, "type"); return; }
                r.Unresolved("unresolved_use", from, "external", Written(n, schema), n);
            }

            // A call: a calls edge when declared of a wanted kind, nothing when declared of another or a #temp procedure, else
            // an unresolved call — an EXEC of a one- or two-part sp_/xp_ or sys-schema name is builtin SYS.<NAME>.
            public void Call(SchemaObjectName name, Kind[] wanted, TSqlFragment at, bool exec)
            {
                if (name?.BaseIdentifier == null || name.BaseIdentifier.Value.StartsWith('#')) return;
                var d = r.Find(scope, schema, name);
                if (d != null) { if (r.Is(d, wanted)) r.Call(Id, d.Id); return; }
                string b = name.BaseIdentifier.Value;
                if (exec && name.DatabaseIdentifier == null && name.ServerIdentifier == null
                    && (b.StartsWith("sp_", StringComparison.OrdinalIgnoreCase) || b.StartsWith("xp_", StringComparison.OrdinalIgnoreCase)
                        || IsSchema(name.SchemaIdentifier?.Value ?? schema, "sys")))
                    r.Unresolved("unresolved_call", Id, "builtin", "SYS." + b.ToUpperInvariant(), at);
                else r.Unresolved("unresolved_call", Id, "external", Written(name, schema), at);
            }

            public void Builtin(string name, TSqlFragment at) => r.Unresolved("unresolved_call", Id, "builtin", name.ToUpperInvariant(), at);
        }

        sealed class Walker : TSqlFragmentVisitor
        {
            readonly Unit unit;
            // types: the unit is a Function (not a view), so type uses and calls start at it.
            readonly bool dml, types, collect;
            readonly HashSet<TSqlFragment> locals, writes;

            public Walker(Unit u, bool dmlTrigger, bool typeUses, bool collecting, HashSet<TSqlFragment> localRefs, HashSet<TSqlFragment> writeRefs)
            {
                unit = u; dml = dmlTrigger; types = typeUses; collect = collecting; locals = localRefs; writes = writeRefs;
            }

            public override void Visit(ExecutableProcedureReference n)
            {
                // A procedure variable has no Name and an EXEC of a string is no ExecutableProcedureReference: both ungraphed.
                if (collect && types) unit.Call(n.ProcedureReference?.ProcedureReference?.Name, Procedures, n, true);
            }

            // A schema-less scalar call is a builtin; a schema-qualified one resolves or is external.
            public override void Visit(FunctionCall f)
            {
                if (!collect || !types) return;
                if (f.CallTarget == null) { unit.Builtin(f.FunctionName.Value, f); return; }
                if (f.CallTarget is not MultiPartIdentifierCallTarget t) return;
                var name = new SchemaObjectName();
                foreach (var i in t.MultiPartIdentifier.Identifiers) name.Identifiers.Add(i);
                name.Identifiers.Add(f.FunctionName);
                unit.Call(name, Functions, f, false);
            }

            // ScriptDom parses LEFT(...) and RIGHT(...) into their own nodes, not FunctionCall; both are builtins too.
            public override void Visit(LeftFunctionCall f)
            {
                if (collect && types) unit.Builtin("LEFT", f);
            }

            public override void Visit(RightFunctionCall f)
            {
                if (collect && types) unit.Builtin("RIGHT", f);
            }

            public override void Visit(SchemaObjectFunctionTableReference n)
            {
                if (collect && types) unit.Call(n.SchemaObject, Functions, n, false);
            }

            public override void Visit(SelectStatement n) { Ctes(n); Read(n.Into); }
            public override void Visit(TruncateTableStatement n) => Read(n.TableName);
            public override void Visit(InsertStatement n) => Ctes(n);
            public override void Visit(UpdateStatement n) => Ctes(n);
            public override void Visit(DeleteStatement n) => Ctes(n);
            public override void Visit(MergeStatement n) => Ctes(n);

            // A one-part reference to a CTE of its own statement is local.
            void Ctes(StatementWithCtesAndXmlNamespaces s)
            {
                if (collect || s.WithCtesAndXmlNamespaces == null) return;
                var names = new HashSet<string>(s.WithCtesAndXmlNamespaces.CommonTableExpressions.Select(c => c.ExpressionName.Value),
                    StringComparer.OrdinalIgnoreCase);
                var refs = new Refs();
                s.Accept(refs);
                foreach (var n in refs.All)
                    if (OnePart(n.SchemaObject) && names.Contains(n.SchemaObject.BaseIdentifier.Value)) locals.Add(n);
            }

            public override void Visit(InsertSpecification n) => Target(n.Target, null);
            public override void Visit(UpdateSpecification n) => Target(n.Target, n.FromClause?.TableReferences);
            public override void Visit(DeleteSpecification n) => Target(n.Target, n.FromClause?.TableReferences);
            public override void Visit(MergeSpecification n) => Target(n.Target, new[] { n.TableReference });

            // A one-part target naming an alias of the FROM clause or table source writes the table it aliases.
            void Target(TableReference target, IEnumerable<TableReference> sources)
            {
                if (collect || target is not NamedTableReference n) return;
                if (OnePart(n.SchemaObject) && sources != null)
                {
                    var aliases = new Dictionary<string, TableReference>(StringComparer.OrdinalIgnoreCase);
                    foreach (var s in sources) Aliases(s, aliases);
                    if (aliases.TryGetValue(n.SchemaObject.BaseIdentifier.Value, out var aliased) && aliased != n)
                    {
                        locals.Add(n);
                        writes.Add(aliased);
                        return;
                    }
                }
                writes.Add(n);
            }

            static void Aliases(TableReference r, Dictionary<string, TableReference> into)
            {
                switch (r)
                {
                    case TableReferenceWithAlias a when a.Alias != null: into.TryAdd(a.Alias.Value, a); break;
                    case JoinTableReference j: Aliases(j.FirstTableReference, into); Aliases(j.SecondTableReference, into); break;
                    case JoinParenthesisTableReference p: Aliases(p.Join, into); break;
                }
            }

            public override void Visit(NamedTableReference n)
            {
                if (!collect || locals.Contains(n)) return;
                string name = n.SchemaObject.BaseIdentifier?.Value;
                if (name == null || name.StartsWith('#')) return;
                if (dml && OnePart(n.SchemaObject)
                    && (name.Equals("inserted", StringComparison.OrdinalIgnoreCase) || name.Equals("deleted", StringComparison.OrdinalIgnoreCase)))
                    return;
                unit.Add(n.SchemaObject, ObjectKinds, writes.Contains(n) ? "write" : "read");
            }

            // SELECT … INTO and TRUNCATE TABLE name an object outside a NamedTableReference: a read (sql-uses-kind); #temp is local.
            void Read(SchemaObjectName n)
            {
                if (collect && !(n?.BaseIdentifier?.Value ?? "#").StartsWith('#')) unit.Add(n, ObjectKinds, "read");
            }

            public override void Visit(NextValueForExpression n)
            {
                if (collect) unit.Add(n.SequenceName, ObjectKinds, "read");
            }

            public override void Visit(ProcedureParameter p)
            {
                if (collect && types) unit.Type(unit.Id, p.DataType);
            }

            // A ProcedureParameter is a DeclareVariableElement too; it is typed once, above.
            public override void Visit(DeclareVariableElement d)
            {
                if (collect && types && d is not ProcedureParameter) unit.Type(unit.Id, d.DataType);
            }

            // DECLARE @t TABLE (...) and a multi-statement TVF's RETURNS @r TABLE (...).
            public override void Visit(DeclareTableVariableBody b)
            {
                if (collect && types) Columns(b.Definition);
            }

            public override void Visit(CreateTableStatement c)
            {
                if (collect && types && (c.SchemaObjectName.BaseIdentifier?.Value ?? "").StartsWith('#')) Columns(c.Definition);
            }

            void Columns(TableDefinition def)
            {
                if (def?.ColumnDefinitions == null) return;
                foreach (var d in def.ColumnDefinitions) unit.Type(unit.Id, d.DataType);
            }
        }

        sealed class Refs : TSqlFragmentVisitor
        {
            public readonly List<NamedTableReference> All = new List<NamedTableReference>();
            public override void Visit(NamedTableReference n) => All.Add(n);
        }
    }
}
