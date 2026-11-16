using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.RegularExpressions;
using System.Xml.Linq;

namespace Apg.SqlFrontend
{
    // .sqlproj reader: unconditioned XML content only (no MSBuild, no Import), then .sql -> database scope assignment.
    public static class SqlProjectReader
    {
        // Item specs are '/'-separated and relative to the project directory, as written.
        public sealed record Project(string Path, bool SdkStyle, string DefaultSchema,
            List<string> BuildIncludes, List<string> BuildRemoves, List<string> Nones,
            List<string> PreDeploys, List<string> PostDeploys);

        // ModuleIdentity: the .sqlproj repo-relative path for a project scope, the repo-relative directory for a loose one.
        public sealed record Scope(string ModuleIdentity, string DefaultSchema, bool IsProject, List<string> Files);

        public const string DefaultSchemaName = "dbo";

        public static Project ParseProject(string path, string xml)
        {
            var root = XDocument.Parse(xml).Root;
            bool sdk = root.Attribute("Sdk") != null || root.Elements().Any(e => e.Name.LocalName == "Sdk");
            string schema = null;
            var lists = new Dictionary<string, List<string>>
            {
                ["Build"] = new List<string>(), ["Build Remove"] = new List<string>(), ["None"] = new List<string>(),
                ["PreDeploy"] = new List<string>(), ["PostDeploy"] = new List<string>(),
            };
            foreach (var group in root.Elements().Where(e => e.Attribute("Condition") == null))
            {
                foreach (var el in group.Elements().Where(e => e.Attribute("Condition") == null))
                {
                    string name = el.Name.LocalName;
                    if (group.Name.LocalName == "PropertyGroup")
                    {
                        if (name == "DefaultSchema" && el.Value.Trim().Length > 0) schema = el.Value.Trim();
                        continue;
                    }
                    if (group.Name.LocalName != "ItemGroup") continue;
                    if (name == "Build") AddSpecs(lists["Build Remove"], el.Attribute("Remove")?.Value);
                    if (lists.ContainsKey(name)) AddSpecs(lists[name], el.Attribute("Include")?.Value);
                }
            }
            return new Project(path, sdk, schema ?? DefaultSchemaName, lists["Build"], lists["Build Remove"],
                lists["None"], lists["PreDeploy"], lists["PostDeploy"]);
        }

        static void AddSpecs(List<string> into, string value)
        {
            if (value == null) return;
            foreach (var s in value.Split(';'))
                if (s.Trim().Length > 0) into.Add(s.Trim().Replace('\\', '/'));
        }

        public static List<Scope> BuildScopes(IEnumerable<Project> projects, IEnumerable<string> sqlPaths)
        {
            var files = sqlPaths.Where(p => p.EndsWith(".sql", StringComparison.OrdinalIgnoreCase))
                .Distinct(StringComparer.Ordinal).OrderBy(p => p, StringComparer.Ordinal).ToList();
            var ordered = projects.OrderBy(p => p.Path, StringComparer.Ordinal).ToList();
            var scopes = ordered.Select(p => new Scope(p.Path, p.DefaultSchema, true, new List<string>())).ToList();
            var matchers = ordered.Select(Membership).ToList();
            var loose = new SortedDictionary<string, Scope>(StringComparer.Ordinal);
            foreach (var file in files)
            {
                int owner = matchers.FindIndex(m => m(file));
                if (owner >= 0) { scopes[owner].Files.Add(file); continue; }
                int slash = file.LastIndexOf('/');
                string dir = slash < 0 ? "" : file.Substring(0, slash);
                if (!loose.TryGetValue(dir, out var scope))
                    loose[dir] = scope = new Scope(dir, DefaultSchemaName, false, new List<string>());
                scope.Files.Add(file);
            }
            scopes.AddRange(loose.Values);
            return scopes;
        }

        static Func<string, bool> Membership(Project p)
        {
            int slash = p.Path.LastIndexOf('/');
            string dir = slash < 0 ? "" : p.Path.Substring(0, slash);
            var include = Patterns(dir, p.BuildIncludes);
            var exclude = Patterns(dir, p.SdkStyle
                ? p.BuildRemoves.Concat(p.Nones).Concat(p.PreDeploys).Concat(p.PostDeploys)
                : p.BuildRemoves);
            return file =>
            {
                bool member = include.Any(m => m(file)) || (p.SdkStyle && UnderOutsideBinObj(dir, file));
                return member && !exclude.Any(m => m(file));
            };
        }

        static bool UnderOutsideBinObj(string dir, string file)
        {
            if (dir.Length > 0 && !file.StartsWith(dir + "/", StringComparison.OrdinalIgnoreCase)) return false;
            var segments = file.Substring(dir.Length == 0 ? 0 : dir.Length + 1).Split('/');
            return !segments.Take(segments.Length - 1).Any(s =>
                s.Equals("bin", StringComparison.OrdinalIgnoreCase) || s.Equals("obj", StringComparison.OrdinalIgnoreCase));
        }

        static List<Func<string, bool>> Patterns(string dir, IEnumerable<string> specs) =>
            specs.Select(s => Pattern(Resolve(dir, s))).ToList();

        // Project-directory-relative spec -> repo-relative, '.' and '..' collapsed.
        static string Resolve(string dir, string spec)
        {
            var parts = new List<string>();
            foreach (var seg in (dir.Length == 0 ? spec : dir + "/" + spec).Split('/'))
            {
                if (seg.Length == 0 || seg == ".") continue;
                if (seg == ".." && parts.Count > 0 && parts[^1] != "..") parts.RemoveAt(parts.Count - 1);
                else parts.Add(seg);
            }
            return string.Join("/", parts);
        }

        static Func<string, bool> Pattern(string pattern)
        {
            if (pattern.IndexOfAny(new[] { '*', '?' }) < 0)
                return file => file.Equals(pattern, StringComparison.OrdinalIgnoreCase);
            var rx = new StringBuilder("^");
            for (int i = 0; i < pattern.Length; i++)
            {
                char c = pattern[i];
                if (c == '*' && i + 1 < pattern.Length && pattern[i + 1] == '*')
                {
                    bool dirPrefix = i + 2 < pattern.Length && pattern[i + 2] == '/';
                    rx.Append(dirPrefix ? "(.*/)?" : ".*");
                    i += dirPrefix ? 2 : 1;
                }
                else if (c == '*') rx.Append("[^/]*");
                else if (c == '?') rx.Append("[^/]");
                else rx.Append(Regex.Escape(c.ToString()));
            }
            var regex = new Regex(rx.Append('$').ToString(), RegexOptions.IgnoreCase | RegexOptions.CultureInvariant);
            return file => regex.IsMatch(file);
        }

        // The only I/O: reads each named .sqlproj (repo-relative to rootDir) as text.
        public static List<Scope> Read(string rootDir, IEnumerable<string> sqlprojPaths, IEnumerable<string> sqlPaths) =>
            BuildScopes(sqlprojPaths.Select(p => ParseProject(p, File.ReadAllText(Path.Combine(rootDir, p)))), sqlPaths);
    }
}
