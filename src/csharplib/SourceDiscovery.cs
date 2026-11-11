using System;
using System.Collections.Generic;
using System.IO;

namespace Apg.CsharpFrontend;

/// <summary>
/// C# source enumeration (`.cs`, `.csx`, `.cshtml`, `.razor`): walks the project root (skipping dot-dirs,
/// <c>bin</c>/<c>obj</c>/<c>node_modules</c>), applies the <c>--module</c>
/// restriction, and filters out exclude-glob matches.
/// </summary>
internal sealed class SourceDiscovery
{
    // Sole owner of the C# discovery extension claim.
    private static readonly HashSet<string> SourceExtensions = new(StringComparer.OrdinalIgnoreCase) { ".cs", ".csx", ".cshtml", ".razor" };

    private readonly List<string> _moduleDirs;
    private readonly List<string> _excludePatterns;

    public SourceDiscovery(List<string> moduleDirs, List<string> excludePatterns)
    {
        _moduleDirs = moduleDirs;
        _excludePatterns = excludePatterns;
    }

    public IEnumerable<string> DiscoverFiles(string root)
    {
        var stack = new Stack<string>();
        stack.Push(root);

        while (stack.Count > 0)
        {
            var current = stack.Pop();
            string[] subDirs;
            string[] files;

            try
            {
                subDirs = Directory.GetDirectories(current);
                files = Directory.GetFiles(current).Where(f => SourceExtensions.Contains(Path.GetExtension(f))).ToArray();
            }
            catch
            {
                continue;
            }

            foreach (var file in files)
            {
                var fullPath = Path.GetFullPath(file);
                if (IsExcluded(fullPath, false)) continue;
                yield return fullPath;
            }

            foreach (var subDir in subDirs)
            {
                if (IsExcluded(subDir, true)) continue;
                stack.Push(subDir);
            }
        }
    }

    /// <summary>
    /// The single skip/scope predicate for C# discovery and the project walk:
    /// exclude-pattern match, the dot-dir/bin/obj/node_modules directory skip,
    /// and the <c>--module</c> restriction on path-segment boundaries (a
    /// directory is in scope when it is, lies inside, or is an ancestor of a
    /// module dir; a file only when it lies inside one).
    /// </summary>
    internal bool IsExcluded(string path, bool isDirectory)
    {
        var full = Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
        foreach (var pat in _excludePatterns)
        {
            if (full.Contains(pat, StringComparison.OrdinalIgnoreCase))
                return true;
        }

        if (isDirectory)
        {
            var name = Path.GetFileName(full);
            if (name.StartsWith('.') || name == "bin" || name == "obj" || name == "node_modules")
                return true;
        }

        if (_moduleDirs.Count == 0) return false;
        foreach (var m in _moduleDirs)
        {
            var mod = Path.TrimEndingDirectorySeparator(Path.GetFullPath(m));
            if (IsWithin(full, mod) || (isDirectory && IsWithin(mod, full)))
                return false;
        }
        return true;
    }

    // `path` equals `dir` or lies under it, matched on a separator boundary.
    private static bool IsWithin(string path, string dir) =>
        path.StartsWith(dir, StringComparison.Ordinal)
        && (path.Length == dir.Length || path[dir.Length] == Path.DirectorySeparatorChar || dir.EndsWith(Path.DirectorySeparatorChar));
}
