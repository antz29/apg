using System;
using System.Collections.Generic;
using System.IO;

namespace Apg.CsharpFrontend;

/// <summary>
/// `*.cs` source enumeration: walks the project root (skipping dot-dirs,
/// <c>bin</c>/<c>obj</c>/<c>node_modules</c>), applies the <c>--module</c>
/// restriction, and filters out exclude-glob matches.
/// </summary>
internal sealed class SourceDiscovery
{
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
                files = Directory.GetFiles(current, "*.cs");
            }
            catch
            {
                continue;
            }

            foreach (var file in files)
            {
                var fullPath = Path.GetFullPath(file);
                if (IsExcluded(fullPath)) continue;
                if (_moduleDirs.Count > 0 && !_moduleDirs.Any(m => fullPath.StartsWith(m, StringComparison.Ordinal))) continue;
                yield return fullPath;
            }

            foreach (var subDir in subDirs)
            {
                var name = Path.GetFileName(subDir);
                if (name.StartsWith('.') || name == "bin" || name == "obj" || name == "node_modules")
                    continue;
                stack.Push(subDir);
            }
        }
    }

    private bool IsExcluded(string path)
    {
        foreach (var pat in _excludePatterns)
        {
            if (path.Contains(pat, StringComparison.OrdinalIgnoreCase))
                return true;
        }
        return false;
    }
}
