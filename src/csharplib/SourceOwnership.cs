namespace Apg.CsharpFrontend;

/// <summary>
/// Picks the one project that owns each compiled source file, so a file
/// several projects compile is emitted (and referenced) under one identity.
/// </summary>
internal static class SourceOwnership
{
    /// <summary>
    /// Maps every source path a loaded project compiles to its owning project:
    /// the sole compiling project, else the compiling project whose directory
    /// contains the file (the nearest when nested), else the first compiling
    /// project in project-path order. A projectless file has no entry. Razor
    /// paths a project records (RazorPaths) are covered by the same rule.
    /// </summary>
    public static Dictionary<string, CsharpProject> AssignOwners(IEnumerable<CsharpProject> projects)
    {
        // Absolute paths under one scan root sort as their repository-relative forms do.
        var compilers = new Dictionary<string, List<CsharpProject>>(StringComparer.Ordinal);
        foreach (var p in projects.OrderBy(p => p.Path, StringComparer.Ordinal))
        {
            foreach (var f in p.SourcePaths.Concat(p.RazorPaths).Distinct(StringComparer.Ordinal))
            {
                if (!compilers.TryGetValue(f, out var list)) compilers[f] = list = new();
                list.Add(p);
            }
        }
        var owners = new Dictionary<string, CsharpProject>(StringComparer.Ordinal);
        foreach (var (file, list) in compilers)
        {
            CsharpProject? owner = null;
            var depth = -1;
            foreach (var p in list)
            {
                var dir = Path.GetDirectoryName(p.Path)! + Path.DirectorySeparatorChar;
                if (dir.Length > depth && file.StartsWith(dir, StringComparison.Ordinal)) (owner, depth) = (p, dir.Length);
            }
            owners[file] = owner ?? list[0];
        }
        return owners;
    }
}
