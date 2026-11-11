using System.Text.Json;
using System.Xml.Linq;
using Microsoft.CodeAnalysis;

namespace Apg.CsharpFrontend;

/// <summary>
/// Offline resolution of a project's package and assembly references to
/// metadata references, from on-disk artefacts only: MSBuild, dotnet and
/// NuGet are never run and no network connection is opened.
/// </summary>
internal static class ReferenceResolver
{
    private static readonly StringComparison PathComparison =
        OperatingSystem.IsWindows() || OperatingSystem.IsMacOS() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;

    /// <summary>
    /// Resolves <paramref name="project"/>'s package and assembly references,
    /// in order: obj/project.assets.json, then the HintPath (assembly) or the
    /// nearest solution packages/ folder up to <paramref name="scanRoot"/>
    /// (package), then the NuGet global packages folder (package). A reference
    /// found nowhere adds nothing. Every dll is loaded through
    /// <see cref="MetadataReferenceCache.Get"/>; the returned list is new per call.
    /// </summary>
    public static List<MetadataReference> Resolve(CsharpProject project, string scanRoot)
    {
        var refs = new List<MetadataReference>();
        var seen = new HashSet<MetadataReference>();
        void Add(string dll)
        {
            try
            {
                var r = MetadataReferenceCache.Get(dll);
                if (seen.Add(r)) refs.Add(r);
            }
            catch (Exception e) when (e is IOException or BadImageFormatException or UnauthorizedAccessException)
            {
                // An unreadable dll stays unresolved, as in GetDefaultMetadataReferences.
            }
        }

        var projectDir = Path.GetDirectoryName(Path.GetFullPath(project.Path))!;
        var tfm = project.TargetFramework;
        var fromAssets = AddAssets(projectDir, tfm, Add) ?? new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        foreach (var (include, hintPath) in project.AssemblyReferences)
        {
            if (string.IsNullOrEmpty(hintPath)) continue;
            var dll = Path.GetFullPath(Path.Combine(projectDir, hintPath.Replace('\\', '/')));
            if (File.Exists(dll)) Add(dll);
        }

        var packagesDir = UpTo(projectDir, scanRoot).Select(d => Path.Combine(d, "packages")).FirstOrDefault(Directory.Exists);
        var globalDir = GlobalPackagesFolder();
        foreach (var (id, written) in project.PackageReferences)
        {
            if (fromAssets.Contains(id)) continue;
            var version = written ?? CentralVersion(projectDir, scanRoot, id);
            if (version == null) continue;
            var predicate = VersionPredicate(version);
            if (predicate == null) continue;

            if (packagesDir != null)
            {
                var candidates = Directory.GetDirectories(packagesDir)
                    .Select(d => (Dir: d, Text: Path.GetFileName(d)))
                    .Where(x => x.Text.StartsWith(id + ".", StringComparison.OrdinalIgnoreCase))
                    .Select(x => (x.Dir, Text: x.Text[(id.Length + 1)..]));
                if (AddPackage(candidates, predicate, tfm, Add)) continue;
            }
            var globalPkg = Path.Combine(globalDir, id.ToLowerInvariant());
            if (Directory.Exists(globalPkg))
                AddPackage(Directory.GetDirectories(globalPkg).Select(d => (d, Path.GetFileName(d))), predicate, tfm, Add);
        }
        return refs;
    }

    /// <summary>Dotnet root for in-process scans; null = APG_CSHARP_DOTNET_ROOT, else the default lookup; set = the only root consulted.</summary>
    internal static string? DotnetRootOverride { get; set; }

    /// <summary>Reference Assemblies root (…\Microsoft\Framework) for in-process scans; null = APG_CSHARP_REFASM_ROOT, else the default lookup; set = the only root consulted.</summary>
    internal static string? ReferenceAssembliesRootOverride { get; set; }

    /// <summary>
    /// Resolves <paramref name="project"/>'s framework references from the
    /// reference assemblies of its target framework on this machine: for
    /// .NET Core/5+ the targeting pack of each explicit or Sdk/TFM-implied
    /// framework (highest installed version with the TFM's major.minor), for
    /// net4x the Reference Assemblies dir, else the
    /// microsoft.netframework.referenceassemblies package. A found set replaces
    /// the carried runtime; when none is found a new copy of the carried
    /// runtime references is returned. <paramref name="dotnetRoot"/> and
    /// <paramref name="referenceAssembliesRoot"/> win over the static
    /// overrides, which win over non-empty APG_CSHARP_DOTNET_ROOT /
    /// APG_CSHARP_REFASM_ROOT, which win over the default lookup; the first
    /// set one is the only root consulted. Offline; every dll goes
    /// through <see cref="MetadataReferenceCache.Get"/>; the list is new per call.
    /// </summary>
    public static List<MetadataReference> ResolveFrameworkReferences(
        CsharpProject project, string? dotnetRoot = null, string? referenceAssembliesRoot = null)
    {
        var refs = new List<MetadataReference>();
        var seen = new HashSet<MetadataReference>();
        void AddDir(string dir)
        {
            foreach (var dll in Directory.GetFiles(dir, "*.dll").OrderBy(f => f, StringComparer.Ordinal))
            {
                try
                {
                    var r = MetadataReferenceCache.Get(dll);
                    if (seen.Add(r)) refs.Add(r);
                }
                catch (Exception e) when (e is IOException or BadImageFormatException or UnauthorizedAccessException)
                {
                    // An unreadable dll stays unresolved, as in GetDefaultMetadataReferences.
                }
            }
        }

        var baseTfm = project.TargetFramework?.Split('-')[0].ToLowerInvariant();
        switch (baseTfm == null ? null : ParseTfm(baseTfm))
        {
            case (Family.Core, var v):
                var frameworks = new List<string>();
                if (project.IsSdkStyle && v >= new Version(3, 0))
                {
                    frameworks.Add("Microsoft.NETCore.App");
                    if (project.Sdk.Trim().Equals("Microsoft.NET.Sdk.Web", StringComparison.OrdinalIgnoreCase))
                        frameworks.Add("Microsoft.AspNetCore.App");
                }
                frameworks.AddRange(project.FrameworkReferences);
                var roots = (dotnetRoot ?? DotnetRootOverride ?? EnvRoot("APG_CSHARP_DOTNET_ROOT")) is { } root
                    ? new[] { root } : DefaultDotnetRoots();
                foreach (var fw in frameworks.Distinct(StringComparer.OrdinalIgnoreCase))
                {
                    // The first root holding a matching pack wins, in lookup order.
                    foreach (var candidateRoot in roots)
                    {
                        var packDir = Path.Combine(candidateRoot, "packs", fw + ".Ref");
                        if (!Directory.Exists(packDir)) continue;
                        var best = Directory.GetDirectories(packDir)
                            .Select(d => (Dir: d, Ver: ParseVersion(Path.GetFileName(d))))
                            .Where(x => x.Ver is { } pv && pv.Number.Major == v.Major && pv.Number.Minor == v.Minor
                                && Directory.Exists(Path.Combine(x.Dir, "ref", baseTfm!)))
                            .OrderByDescending(x => x.Ver!.Value)
                            .Select(x => x.Dir)
                            .FirstOrDefault();
                        if (best == null) continue;
                        AddDir(Path.Combine(best, "ref", baseTfm!));
                        break;
                    }
                }
                break;
            case (Family.Framework, var v):
                var ver = "v" + v;
                var refAsmRoot = referenceAssembliesRoot ?? ReferenceAssembliesRootOverride
                    ?? EnvRoot("APG_CSHARP_REFASM_ROOT") ?? DefaultReferenceAssembliesRoot();
                var dir = refAsmRoot == null ? null : Path.Combine(refAsmRoot, ".NETFramework", ver);
                if (dir != null && Directory.Exists(dir))
                {
                    AddDir(dir);
                    break;
                }
                var pkg = Path.Combine(GlobalPackagesFolder(), "microsoft.netframework.referenceassemblies." + baseTfm);
                if (!Directory.Exists(pkg)) break;
                var pkgDir = Directory.GetDirectories(pkg)
                    .Select(d => (Dir: d, Ver: ParseVersion(Path.GetFileName(d))))
                    .Where(x => x.Ver != null)
                    .OrderByDescending(x => x.Ver!.Value)
                    .Select(x => Path.Combine(x.Dir, "build", ".NETFramework", ver))
                    .FirstOrDefault(Directory.Exists);
                if (pkgDir != null) AddDir(pkgDir);
                break;
        }
        // Never alongside the carried runtime: duplicate System assemblies make System types ambiguous.
        return refs.Count > 0 ? refs : new List<MetadataReference>(CompilationLoader.GetDefaultMetadataReferences());
    }

    /// <summary>A root override env var reaching the spawned frontend process; unset or empty = null.</summary>
    private static string? EnvRoot(string name) =>
        Environment.GetEnvironmentVariable(name) is { Length: > 0 } v ? v : null;

    /// <summary>DOTNET_ROOT, then the platform default install dirs.</summary>
    private static IEnumerable<string> DefaultDotnetRoots()
    {
        if (Environment.GetEnvironmentVariable("DOTNET_ROOT") is { Length: > 0 } env) yield return env;
        if (OperatingSystem.IsWindows())
        {
            yield return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "dotnet");
            yield break;
        }
        yield return "/usr/local/share/dotnet";
        yield return "/usr/share/dotnet";
        yield return "/usr/lib/dotnet";
        yield return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".dotnet");
    }

    /// <summary>%ProgramFiles(x86)%\Reference Assemblies\Microsoft\Framework, or null when unset.</summary>
    private static string? DefaultReferenceAssembliesRoot() =>
        Environment.GetEnvironmentVariable("ProgramFiles(x86)") is { Length: > 0 } pf
            ? Path.Combine(pf, "Reference Assemblies", "Microsoft", "Framework")
            : null;

    private static string GlobalPackagesFolder() =>
        Environment.GetEnvironmentVariable("NUGET_PACKAGES") is { Length: > 0 } env
            ? env
            : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".nuget", "packages");

    /// <summary>The directory and each ancestor, up to and including <paramref name="scanRoot"/>.</summary>
    private static IEnumerable<string> UpTo(string dir, string scanRoot)
    {
        var root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(scanRoot));
        for (var d = Path.GetFullPath(dir); d != null; d = Path.GetDirectoryName(d))
        {
            yield return d;
            if (string.Equals(Path.TrimEndingDirectorySeparator(d), root, PathComparison)) yield break;
        }
    }

    /// <summary>PackageVersion of <paramref name="id"/> in the nearest Directory.Packages.props, or null.</summary>
    private static string? CentralVersion(string projectDir, string scanRoot, string id)
    {
        var props = UpTo(projectDir, scanRoot).Select(d => Path.Combine(d, "Directory.Packages.props")).FirstOrDefault(File.Exists);
        if (props == null) return null;
        try
        {
            return XDocument.Load(props).Descendants()
                .Where(e => e.Name.LocalName == "PackageVersion"
                    && string.Equals(e.Attribute("Include")?.Value, id, StringComparison.OrdinalIgnoreCase))
                .Select(e => e.Attribute("Version")?.Value ?? e.Elements().FirstOrDefault(c => c.Name.LocalName == "Version")?.Value)
                .FirstOrDefault(v => !string.IsNullOrWhiteSpace(v))?.Trim();
        }
        catch (Exception e) when (e is IOException or System.Xml.XmlException)
        {
            return null;
        }
    }

    /// <summary>
    /// Adds the compile assets of every package in the assets file's target for
    /// <paramref name="tfm"/>; returns the ids fully found on disk, or null when
    /// there is no usable assets file or matching target.
    /// </summary>
    private static HashSet<string>? AddAssets(string projectDir, string? tfm, Action<string> add)
    {
        var file = Path.Combine(projectDir, "obj", "project.assets.json");
        if (tfm == null || !File.Exists(file)) return null;
        try
        {
            using var doc = JsonDocument.Parse(File.ReadAllBytes(file));
            var root = doc.RootElement;
            if (!root.TryGetProperty("targets", out var targets) || targets.ValueKind != JsonValueKind.Object) return null;
            var keys = new List<string> { tfm };
            if (LongForm(tfm) is { } longForm) keys.Add(longForm);
            JsonElement? target = null;
            foreach (var t in targets.EnumerateObject())
            {
                if (keys.Contains(t.Name, StringComparer.OrdinalIgnoreCase)) { target = t.Value; break; }
            }
            if (target is not { ValueKind: JsonValueKind.Object } packages) return null;

            var folders = root.TryGetProperty("packageFolders", out var pf) && pf.ValueKind == JsonValueKind.Object
                ? pf.EnumerateObject().Select(p => p.Name).ToList()
                : new List<string>();
            var libraries = root.TryGetProperty("libraries", out var l) && l.ValueKind == JsonValueKind.Object ? l : default;
            var resolved = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var pkg in packages.EnumerateObject())
            {
                if (!pkg.Value.TryGetProperty("type", out var type) || type.GetString() != "package") continue;
                var missing = false;
                if (pkg.Value.TryGetProperty("compile", out var compile) && compile.ValueKind == JsonValueKind.Object)
                {
                    string? libPath = libraries.ValueKind == JsonValueKind.Object
                        && libraries.TryGetProperty(pkg.Name, out var lib)
                        && lib.TryGetProperty("path", out var p) ? p.GetString() : null;
                    foreach (var entry in compile.EnumerateObject())
                    {
                        if (Path.GetFileName(entry.Name) == "_._") continue;
                        var found = libPath == null ? null : folders
                            .Select(f => Path.Combine(f, libPath, entry.Name))
                            .FirstOrDefault(File.Exists);
                        if (found != null) add(found);
                        else missing = true;
                    }
                }
                if (!missing) resolved.Add(pkg.Name.Split('/')[0]);
            }
            return resolved;
        }
        catch (Exception e) when (e is JsonException or IOException or InvalidOperationException)
        {
            return null;
        }
    }

    /// <summary>Picks the highest present version satisfying the predicate and adds its compile folder's dlls.</summary>
    private static bool AddPackage(
        IEnumerable<(string Dir, string Text)> candidates, Func<string, NuVer, bool> predicate, string? tfm, Action<string> add)
    {
        var best = candidates
            .Select(c => (c.Dir, c.Text, Ver: ParseVersion(c.Text)))
            .Where(c => c.Ver is { } v && predicate(c.Text, v))
            .OrderByDescending(c => c.Ver!.Value)
            .Select(c => c.Dir)
            .FirstOrDefault();
        if (best == null) return false;
        var folder = ChooseFolder(Path.Combine(best, "ref"), tfm) ?? ChooseFolder(Path.Combine(best, "lib"), tfm);
        if (folder == null) return false;
        foreach (var dll in Directory.GetFiles(folder, "*.dll").OrderBy(f => f, StringComparer.Ordinal)) add(dll);
        return true;
    }

    private enum Family { Core, Framework, Standard }

    /// <summary>
    /// One framework folder of a ref/ or lib/ tree: equal to the target
    /// framework, else the highest not-above version of its family, else the
    /// highest netstandard; null when none is compatible.
    /// </summary>
    private static string? ChooseFolder(string treeDir, string? tfm)
    {
        if (!Directory.Exists(treeDir)) return null;
        var dirs = Directory.GetDirectories(treeDir);
        if (tfm != null && dirs.FirstOrDefault(d => Path.GetFileName(d).Equals(tfm, StringComparison.OrdinalIgnoreCase)) is { } exact)
            return exact;
        var parsed = dirs.Select(d => (Dir: d, Tfm: ParseTfm(Path.GetFileName(d)))).Where(x => x.Tfm != null).ToList();
        string? Best(Family family, Version? max) => parsed
            .Where(x => x.Tfm!.Value.Family == family && (max == null || x.Tfm.Value.Version <= max))
            .OrderByDescending(x => x.Tfm!.Value.Version)
            .Select(x => x.Dir)
            .FirstOrDefault();
        // A platform suffix (net8.0-windows) falls back like its base framework.
        if (tfm != null && ParseTfm(tfm.Split('-')[0]) is { } project && Best(project.Family, project.Version) is { } same)
            return same;
        return Best(Family.Standard, null);
    }

    /// <summary>net8.0/netcoreapp3.1 → Core, net472 → Framework 4.7.2, netstandard2.0 → Standard; null otherwise.</summary>
    private static (Family Family, Version Version)? ParseTfm(string tfm)
    {
        var t = tfm.ToLowerInvariant();
        if (t.Contains('-')) return null;
        (Family, Version)? Of(Family family, string v) => Version.TryParse(v, out var ver) ? (family, ver) : null;
        if (t.StartsWith("netstandard")) return Of(Family.Standard, t[11..]);
        if (t.StartsWith("netcoreapp")) return Of(Family.Core, t[10..]);
        if (!t.StartsWith("net")) return null;
        var rest = t[3..];
        if (rest.Contains('.')) return Of(Family.Core, rest) is { } core && core.Item2.Major >= 5 ? core : null;
        return rest.Length >= 2 && rest.All(char.IsAsciiDigit) ? Of(Family.Framework, string.Join('.', rest.ToCharArray())) : null;
    }

    /// <summary>The assets-file long form of a short target framework (net8.0 → .NETCoreApp,Version=v8.0).</summary>
    private static string? LongForm(string tfm) => ParseTfm(tfm.Split('-')[0]) switch
    {
        (Family.Core, var v) => $".NETCoreApp,Version=v{v.Major}.{v.Minor}",
        (Family.Standard, var v) => $".NETStandard,Version=v{v.Major}.{v.Minor}",
        (Family.Framework, var v) => $".NETFramework,Version=v{v}",
        _ => null,
    };

    /// <summary>A NuGet version: numeric part plus prerelease label (build metadata dropped).</summary>
    private readonly record struct NuVer(Version Number, string Pre) : IComparable<NuVer>
    {
        public int CompareTo(NuVer other)
        {
            var c = Pad(Number).CompareTo(Pad(other.Number));
            if (c != 0) return c;
            // A release sorts above any prerelease of the same number.
            if (Pre.Length == 0 || other.Pre.Length == 0) return (Pre.Length == 0).CompareTo(other.Pre.Length == 0);
            return string.Compare(Pre, other.Pre, StringComparison.OrdinalIgnoreCase);
        }

        // 1.0 and 1.0.0 are the same version.
        private static Version Pad(Version v) => new(v.Major, v.Minor, Math.Max(v.Build, 0), Math.Max(v.Revision, 0));
    }

    private static NuVer? ParseVersion(string text)
    {
        var s = text.Trim();
        if (s.IndexOf('+') is var plus and >= 0) s = s[..plus];
        var dash = s.IndexOf('-');
        var number = dash >= 0 ? s[..dash] : s;
        if (!number.Contains('.')) number += ".0";
        return Version.TryParse(number, out var v) ? new NuVer(v, dash >= 0 ? s[(dash + 1)..] : "") : null;
    }

    /// <summary>
    /// The version filter for a reference's Version: a plain version matches
    /// exactly, a range ([1.0,2.0), (,3.0], [1.0]) by its bounds, a float
    /// (*, 1.*, 1.2.*) by prefix; null when unparseable.
    /// </summary>
    private static Func<string, NuVer, bool>? VersionPredicate(string spec)
    {
        spec = spec.Trim();
        if (spec.Length == 0) return null;
        if (spec.IndexOf('*') is var star and >= 0)
        {
            var prefix = spec[..star];
            var allowPre = prefix.Contains('-');
            return (text, v) => text.StartsWith(prefix, StringComparison.OrdinalIgnoreCase) && (allowPre || v.Pre.Length == 0);
        }
        if (spec[0] is '[' or '(')
        {
            if (spec.Length < 2 || spec[^1] is not (']' or ')')) return null;
            bool lowInc = spec[0] == '[', highInc = spec[^1] == ']';
            var parts = spec[1..^1].Split(',');
            if (parts.Length == 1)
                return lowInc && highInc && ParseVersion(parts[0]) is { } only ? (_, v) => v.CompareTo(only) == 0 : null;
            if (parts.Length != 2) return null;
            NuVer? low = null, high = null;
            if (parts[0].Trim().Length > 0 && (low = ParseVersion(parts[0])) == null) return null;
            if (parts[1].Trim().Length > 0 && (high = ParseVersion(parts[1])) == null) return null;
            return (_, v) =>
                (low is not { } lo || (lowInc ? v.CompareTo(lo) >= 0 : v.CompareTo(lo) > 0))
                && (high is not { } hi || (highInc ? v.CompareTo(hi) <= 0 : v.CompareTo(hi) < 0));
        }
        return ParseVersion(spec) is { } exactVersion ? (_, v) => v.CompareTo(exactVersion) == 0 : null;
    }
}
