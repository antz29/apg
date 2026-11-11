namespace Apg.CsharpFrontend;

/// <summary>
/// One statically read <c>.csproj</c>: plain data, filled by
/// <c>ProjectFileReader.Read</c>. Values are kept as written in the project
/// file; resolving them is the consumers' job.
/// </summary>
internal sealed class CsharpProject
{
    /// <summary>Absolute path of the project file.</summary>
    public string Path { get; set; } = "";

    /// <summary>Project name (the project file name without extension).</summary>
    public string Name { get; set; } = "";

    /// <summary>The RootNamespace property, or null when not set.</summary>
    public string? RootNamespace { get; set; }

    /// <summary>True for an SDK-style project, false for .NET Framework-style.</summary>
    public bool IsSdkStyle { get; set; }

    /// <summary>
    /// The Sdk value as written (e.g. Microsoft.NET.Sdk.Web); empty for a
    /// .NET Framework-style project.
    /// </summary>
    public string Sdk { get; set; } = "";

    /// <summary>Absolute paths of the compiled source files.</summary>
    public List<string> SourcePaths { get; } = new();

    /// <summary>ProjectReference paths as written.</summary>
    public List<string> ProjectReferences { get; } = new();

    /// <summary>PackageReference (or packages.config) Include and Version as written.</summary>
    public List<(string Include, string? Version)> PackageReferences { get; } = new();

    /// <summary>Reference Include and HintPath as written.</summary>
    public List<(string Include, string? HintPath)> AssemblyReferences { get; } = new();

    /// <summary>FrameworkReference Include values as written.</summary>
    public List<string> FrameworkReferences { get; } = new();

    /// <summary>DefineConstants preprocessor symbols.</summary>
    public List<string> PreprocessorSymbols { get; } = new();

    /// <summary>The LangVersion property as written, or null when not set.</summary>
    public string? LanguageVersion { get; set; }

    /// <summary>The first listed TargetFramework/TargetFrameworks entry, or null.</summary>
    public string? TargetFramework { get; set; }
}
