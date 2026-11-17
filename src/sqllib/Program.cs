using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;

namespace Apg.SqlFrontend
{
    public static class Program
    {
        // Writes only through Console.Out/Console.Error (read at call time), so tests can call Main in-process.
        public static int Main(string[] args)
        {
            const string usage = "Usage: sqlfrontend <project_dir> --targets <file> [--module <dir>]... [--id-prefix <p>] [--cache-dir <dir>] [--cache-key <key>] [exclude_globs...]";
            if (args.Length == 0 || args[0] == "-h" || args[0] == "--help")
            {
                Console.Error.WriteLine(usage);
                return 1;
            }

            string rootDir = Path.TrimEndingDirectorySeparator(Path.GetFullPath(args[0])).Replace('\\', '/');
            var moduleDirs = new List<string>();
            var excludePatterns = new List<string>();
            string idPrefix = "n";
            string targetsPath = null;

            int i = 1;
            while (i < args.Length)
            {
                string a = args[i];
                if (a == "--module" || a == "--id-prefix" || a == "--targets" || a == "--cache-dir" || a == "--cache-key")
                {
                    i++;
                    if (i < args.Length)
                    {
                        if (a == "--module") moduleDirs.Add(Path.GetFullPath(Path.Combine(rootDir, args[i])));
                        else if (a == "--id-prefix") idPrefix = args[i];
                        else if (a == "--targets") targetsPath = args[i];
                        // --cache-dir / --cache-key: accepted for the spawn contract, unused (no per-language artifacts).
                    }
                }
                else if (a.StartsWith("--", StringComparison.Ordinal))
                {
                    // A misspelled flag must fail loudly, never become an exclude pattern.
                    Console.Error.WriteLine(usage);
                    Console.Error.WriteLine($"unknown option {a}");
                    return 1;
                }
                else
                {
                    excludePatterns.Add(a);
                }
                i++;
            }

            // sqlfrontend never walks: the --targets list is the whole emission universe.
            if (targetsPath == null)
            {
                Console.Error.WriteLine(usage);
                Console.Error.WriteLine("missing --targets <file>");
                return 1;
            }

            try
            {
                var targets = File.ReadAllLines(targetsPath, Encoding.UTF8).Where(l => !string.IsNullOrWhiteSpace(l)).ToList();
                SchemaEmitter.Run(rootDir, targets, moduleDirs, excludePatterns, idPrefix, Console.Out, Console.Error);
                return 0;
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"sqlfrontend error: {ex}");
                return 1;
            }
        }
    }
}
