using System.Text.RegularExpressions;

namespace Apg.CsharpFrontend;

/// <summary>
/// Statically reads a <c>.sln</c> (plain text, no MSBuild evaluation) into the
/// absolute paths of the C# projects it lists. Solution folders and non-C#
/// projects (.vbproj, .vcxproj, …) are skipped.
/// </summary>
internal static class SolutionReader
{
    private const string SolutionFolderType = "2150E333-8FDC-42A3-9474-1A3956D46DE8";

    // Project("{TypeGuid}") = "Name", "rel\path.csproj", "{ProjectGuid}"
    private static readonly Regex ProjectLine = new(
        @"^\s*Project\(""\{([^}]*)\}""\)\s*=\s*""[^""]*""\s*,\s*""([^""]*)""",
        RegexOptions.CultureInvariant);

    public static List<string> Read(string slnPath)
    {
        var dir = Path.GetDirectoryName(Path.GetFullPath(slnPath))!;
        var projects = new List<string>();
        foreach (var line in File.ReadLines(slnPath)) // StreamReader strips a BOM; ReadLines splits CRLF
        {
            var m = ProjectLine.Match(line);
            if (!m.Success || string.Equals(m.Groups[1].Value, SolutionFolderType, StringComparison.OrdinalIgnoreCase))
                continue;
            var rel = m.Groups[2].Value.Replace('\\', Path.DirectorySeparatorChar);
            if (!rel.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase)) continue;
            var abs = Path.GetFullPath(Path.Combine(dir, rel));
            if (!projects.Contains(abs)) projects.Add(abs);
        }
        return projects;
    }
}
