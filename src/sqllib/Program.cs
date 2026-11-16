using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace Apg.SqlFrontend
{
    public static class Program
    {
        public static int Main(string[] args)
        {
            if (args.Length != 1)
            {
                Console.Error.WriteLine("usage: sqlfrontend <file.sql>");
                return 2;
            }
            var path = args[0];
            // SQLCMD lines (:r, :setvar, ...) are not T-SQL; blank them so line numbers stay put.
            var lines = File.ReadAllText(path).Split('\n')
                .Select(l => l.TrimStart().StartsWith(":") ? "" : l);
            var text = string.Join("\n", lines);

            var fragment = new TSql170Parser(true).Parse(new StringReader(text), out IList<ParseError> errors);
            foreach (var e in errors)
                Console.Error.WriteLine($"{path}:{e.Line}:{e.Column}: {e.Message}");
            if (errors.Count > 0)
                return 1;

            foreach (var stmt in ((TSqlScript)fragment).Batches.SelectMany(b => b.Statements))
            {
                SchemaObjectName name = stmt switch
                {
                    CreateTableStatement s => s.SchemaObjectName,
                    CreateViewStatement s => s.SchemaObjectName,
                    CreateProcedureStatement s => s.ProcedureReference.Name,
                    CreateFunctionStatement s => s.Name,
                    CreateTriggerStatement s => s.Name,
                    CreateTypeStatement s => s.Name,
                    CreateSequenceStatement s => s.Name,
                    CreateSynonymStatement s => s.Name,
                    _ => null,
                };
                if (name != null)
                    Console.WriteLine($"{stmt.GetType().Name} {string.Join(".", name.Identifiers.Select(i => i.Value))} {stmt.StartLine}");
            }
            return 0;
        }
    }
}
