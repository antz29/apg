using System;
using System.Collections.Concurrent;
using System.IO;
using Microsoft.CodeAnalysis;

namespace Apg.CsharpFrontend;

/// <summary>
/// The single process-wide path-to-<see cref="MetadataReference"/> cache for
/// every resolver-loaded dll. One csharpfrontend process runs one scan, so the
/// cache lives for exactly one scan; entries are never evicted or reloaded.
/// Returning the same instance per file lets Roslyn share that assembly's
/// symbols across every compilation that references it.
/// </summary>
internal static class MetadataReferenceCache
{
    // Windows and macOS file systems are case-insensitive by default.
    private static readonly ConcurrentDictionary<string, Lazy<MetadataReference>> Cache = new(
        OperatingSystem.IsWindows() || OperatingSystem.IsMacOS()
            ? StringComparer.OrdinalIgnoreCase
            : StringComparer.Ordinal);

    public static MetadataReference Get(string dllPath)
    {
        var key = Path.GetFullPath(dllPath);
        // Lazy guarantees CreateFromFile runs once even when GetOrAdd races.
        return Cache.GetOrAdd(key, k => new Lazy<MetadataReference>(() => MetadataReference.CreateFromFile(k))).Value;
    }
}
