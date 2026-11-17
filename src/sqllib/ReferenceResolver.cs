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
        static readonly IReadOnlyDictionary<ColumnDefinition, string> NoColumnIds = new Dictionary<ColumnDefinition, string>();

        readonly Dictionary<SqlProjectReader.Scope, Dictionary<string, SchemaEmitter.Declaration>> index;
        readonly IReadOnlyDictionary<string, Kind> kinds;
        readonly List<(string From, string To, string Kind)> uses = new List<(string, string, string)>();
        // (from, to) -> its position in uses: a repeat only raises the kind (read < write < trigger).
        readonly Dictionary<(string, string), int> positions = new Dictionary<(string, string), int>();

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

        public void Flush(TextWriter stdout)
        {
            foreach (var (from, to, kind) in uses)
                stdout.WriteLine(JsonSerializer.Serialize(new { type = "uses", from, to, properties = new { kind } }));
        }

        static int Rank(string kind) => kind == "trigger" ? 2 : kind == "write" ? 1 : 0;

        // A one- or two-part name of a wanted kind in the unit's own scope; three- and four-part names never resolve.
        SchemaEmitter.Declaration Resolve(SqlProjectReader.Scope scope, string defaultSchema, SchemaObjectName name, Kind[] wanted)
        {
            if (name?.BaseIdentifier == null || name.ServerIdentifier != null || name.DatabaseIdentifier != null) return null;
            if (!index.TryGetValue(scope, out var decls)
                || !decls.TryGetValue((name.SchemaIdentifier?.Value ?? defaultSchema) + "." + name.BaseIdentifier.Value, out var d))
                return null;
            return kinds.TryGetValue(d.Id, out var k) && wanted.Contains(k) ? d : null;
        }

        void Edge(string from, string to, string kind)
        {
            if (!positions.TryGetValue((from, to), out int i)) { positions[(from, to)] = uses.Count; uses.Add((from, to, kind)); }
            else if (Rank(kind) > Rank(uses[i].Kind)) uses[i] = (from, to, kind);
        }

        static bool OnePart(SchemaObjectName n) => n.SchemaIdentifier == null && n.DatabaseIdentifier == null && n.ServerIdentifier == null;

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

            public void Add(SchemaObjectName name, Kind[] wanted, string kind)
            {
                var d = r.Resolve(scope, schema, name, wanted);
                if (d != null) r.Edge(Id, d.Id, kind);
            }

            public void Type(string from, DataTypeReference type)
            {
                var d = type is UserDataTypeReference u ? r.Resolve(scope, schema, u.Name, TypeKinds) : null;
                if (d != null) r.Edge(from, d.Id, "type");
            }
        }

        sealed class Walker : TSqlFragmentVisitor
        {
            readonly Unit unit;
            readonly bool dml, types, collect;
            readonly HashSet<TSqlFragment> locals, writes;

            public Walker(Unit u, bool dmlTrigger, bool typeUses, bool collecting, HashSet<TSqlFragment> localRefs, HashSet<TSqlFragment> writeRefs)
            {
                unit = u; dml = dmlTrigger; types = typeUses; collect = collecting; locals = localRefs; writes = writeRefs;
            }

            public override void Visit(SelectStatement n) => Ctes(n);
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
