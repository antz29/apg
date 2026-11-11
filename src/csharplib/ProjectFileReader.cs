using System.Text.RegularExpressions;
using System.Xml.Linq;

namespace Apg.CsharpFrontend;

/// <summary>
/// Statically reads a <c>.csproj</c> (plain XML, no MSBuild evaluation) into a
/// <see cref="CsharpProject"/>. Only unconditional properties and items are
/// read: a Condition (or Choose/When) needs an evaluated Configuration or
/// TargetFramework, which a static reader does not have.
/// </summary>
internal static class ProjectFileReader
{
    public static CsharpProject Read(string projectPath)
    {
        var path = Path.GetFullPath(projectPath);
        var dir = Path.GetDirectoryName(path)!;
        var root = XDocument.Load(path).Root!;
        var p = new CsharpProject { Path = path, Name = Path.GetFileNameWithoutExtension(path) };

        p.Sdk = root.Attribute("Sdk")?.Value
            ?? root.Elements().FirstOrDefault(e => e.Name.LocalName == "Sdk")?.Attribute("Name")?.Value
            ?? "";
        p.IsSdkStyle = p.Sdk.Length > 0;

        string? Prop(string name) => Props(root, name).LastOrDefault(); // MSBuild: last definition wins
        p.RootNamespace = Prop("RootNamespace");
        p.LanguageVersion = Prop("LangVersion");
        // A set TargetFramework wins over TargetFrameworks, as in MSBuild.
        p.TargetFramework = SplitList(Prop("TargetFramework")).FirstOrDefault()
            ?? SplitList(Prop("TargetFrameworks")).FirstOrDefault();
        foreach (var symbol in Props(root, "DefineConstants").SelectMany(SplitList))
            if (!p.PreprocessorSymbols.Contains(symbol)) p.PreprocessorSymbols.Add(symbol);

        // Compile items: SDK default glob (minus the project's bin/ and obj/), explicit Includes, then Removes.
        var compile = Items(root, "Compile").ToList();
        var sources = new List<string>();
        var removes = new List<Regex>();
        if (p.IsSdkStyle && !string.Equals(Prop("EnableDefaultCompileItems"), "false", StringComparison.OrdinalIgnoreCase))
        {
            sources.AddRange(Expand(dir, "**/*.cs"));
            removes.Add(GlobRegex(Abs(dir, "bin/**")));
            removes.Add(GlobRegex(Abs(dir, "obj/**")));
        }
        foreach (var c in compile)
            foreach (var pattern in SplitList(c.Attribute("Include")?.Value))
                sources.AddRange(Expand(dir, pattern));
        foreach (var c in compile)
            foreach (var pattern in SplitList(c.Attribute("Remove")?.Value))
                removes.Add(GlobRegex(Abs(dir, pattern)));
        p.SourcePaths.AddRange(sources.Distinct().Where(s => !removes.Any(r => r.IsMatch(Norm(s)))));

        foreach (var r in Items(root, "ProjectReference"))
            if (r.Attribute("Include")?.Value is { Length: > 0 } include) p.ProjectReferences.Add(include);
        foreach (var r in Items(root, "PackageReference"))
            if (r.Attribute("Include")?.Value is { Length: > 0 } include)
                p.PackageReferences.Add((include, r.Attribute("Version")?.Value ?? Child(r, "Version")));
        foreach (var r in Items(root, "FrameworkReference"))
            if (r.Attribute("Include")?.Value is { Length: > 0 } include) p.FrameworkReferences.Add(include);
        foreach (var r in Items(root, "Reference"))
            if (r.Attribute("Include")?.Value is { Length: > 0 } include) p.AssemblyReferences.Add((include, Child(r, "HintPath")));

        var packagesConfig = Path.Combine(dir, "packages.config");
        if (File.Exists(packagesConfig))
            foreach (var pkg in XDocument.Load(packagesConfig).Descendants().Where(e => e.Name.LocalName == "package"))
                if (pkg.Attribute("id")?.Value is { Length: > 0 } id) p.PackageReferences.Add((id, pkg.Attribute("version")?.Value));

        return p;
    }

    private static bool Unconditional(XElement e) =>
        e.AncestorsAndSelf().All(a => a.Attribute("Condition") == null && a.Name.LocalName is not ("When" or "Otherwise"));

    private static IEnumerable<string> Props(XElement root, string name) =>
        root.Descendants()
            .Where(e => e.Name.LocalName == name && e.Parent?.Name.LocalName == "PropertyGroup" && Unconditional(e))
            .Select(e => e.Value.Trim());

    private static IEnumerable<XElement> Items(XElement root, string name) =>
        root.Descendants().Where(e => e.Name.LocalName == name && e.Parent?.Name.LocalName == "ItemGroup" && Unconditional(e));

    private static string? Child(XElement e, string name) =>
        e.Elements().FirstOrDefault(c => c.Name.LocalName == name)?.Value.Trim();

    // `;`-separated MSBuild list; property references such as $(DefineConstants) cannot be evaluated statically.
    private static IEnumerable<string> SplitList(string? value) =>
        (value ?? "").Split(';').Select(s => s.Trim()).Where(s => s.Length > 0 && !s.Contains("$("));

    private static string Abs(string dir, string pattern) => Norm(Path.GetFullPath(Path.Combine(dir, pattern.Replace('\\', '/'))));

    private static string Norm(string path) => path.Replace('\\', '/');

    /// <summary>Existing files an Include pattern names (absolute, sorted); a literal path yields itself when present.</summary>
    private static IEnumerable<string> Expand(string dir, string pattern)
    {
        var abs = Abs(dir, pattern);
        if (abs.IndexOfAny(['*', '?']) < 0)
            return File.Exists(abs) ? [Path.GetFullPath(abs)] : [];
        var segments = abs.Split('/');
        var fixedCount = Array.FindIndex(segments, s => s.IndexOfAny(['*', '?']) >= 0);
        var baseDir = string.Join('/', segments.Take(fixedCount));
        if (baseDir.Length == 0) baseDir = "/";
        if (!Directory.Exists(baseDir)) return [];
        var re = GlobRegex(abs);
        var options = new EnumerationOptions { RecurseSubdirectories = true, IgnoreInaccessible = true };
        return Directory.EnumerateFiles(baseDir, "*", options)
            .Where(f => re.IsMatch(Norm(f)))
            .Select(Path.GetFullPath)
            .OrderBy(f => f, StringComparer.Ordinal);
    }

    /// <summary>Anchored regex for a `/`-separated glob: `**/` any directories, `**` anything, `*` and `?` within a segment.</summary>
    private static Regex GlobRegex(string glob)
    {
        var sb = new System.Text.StringBuilder("^");
        for (var i = 0; i < glob.Length; i++)
        {
            if (glob[i] == '*' && i + 1 < glob.Length && glob[i + 1] == '*')
            {
                var slash = i + 2 < glob.Length && glob[i + 2] == '/';
                sb.Append(slash ? "(?:.*/)?" : ".*");
                i += slash ? 2 : 1;
            }
            else if (glob[i] == '*') sb.Append("[^/]*");
            else if (glob[i] == '?') sb.Append("[^/]");
            else sb.Append(Regex.Escape(glob[i].ToString()));
        }
        return new Regex(sb.Append('$').ToString(), RegexOptions.CultureInvariant);
    }
}
