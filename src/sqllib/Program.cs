using System;
using System.IO;
using System.Linq;
using Microsoft.SqlServer.TransactSql.ScriptDom;

namespace Apg.SqlFrontend
{
    public static class Program
    {
        public static int Main(string[] args)
        {
            if (args.Length == 0)
            {
                Console.Error.WriteLine("usage: sqlfrontend <file.sql>...");
                return 2;
            }
            foreach (var path in args)
            {
                var result = SqlSourceReader.Read(path, File.ReadAllText(path));
                // Syntax diagnostics never abort the scan: report and keep going.
                foreach (var d in result.Diagnostics)
                    Console.Error.WriteLine(d);

                foreach (var pb in result.Batches)
                {
                    if (pb.Fragment is not TSqlScript script)
                        continue;
                    foreach (var stmt in script.Batches.SelectMany(b => b.Statements))
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
                        // Fragment positions are batch-relative; shift to the original-file line.
                        if (name != null)
                            Console.WriteLine($"{stmt.GetType().Name} {string.Join(".", name.Identifiers.Select(i => i.Value))} {pb.Batch.StartLine + stmt.StartLine - 1}");
                    }
                }
            }
            return 0;
        }
    }
}
