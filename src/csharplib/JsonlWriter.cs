using System;
using System.Text.Json;

namespace Apg.CsharpFrontend;

/// <summary>
/// JSONL transport: serialize one unified-schema record per line to stdout.
/// </summary>
internal static class JsonlWriter
{
    /// <summary>
    /// Writes one record as a single JSON line. Resolves <see cref="Console.Out"/>
    /// at call time (never captures a writer in a ctor): the test harness
    /// redirects <c>Console.SetOut</c> after constructing the Scanner.
    /// </summary>
    public static void Write(object obj)
    {
        var json = JsonSerializer.Serialize(obj);
        Console.WriteLine(json);
    }
}
