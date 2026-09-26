using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace Apg.CsharpFrontend;

// --- Unified JSONL Schema records (SPEC §2) ---

public class ModuleMsg
{
    [JsonPropertyName("type")]
    public string Type => "module";

    [JsonPropertyName("fqn")]
    public string Fqn { get; set; } = "";
}

public class FileMsg
{
    [JsonPropertyName("type")]
    public string Type => "file";

    [JsonPropertyName("path")]
    public string Path { get; set; } = "";

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = "";

    [JsonPropertyName("start_line")]
    public int StartLine { get; set; }

    [JsonPropertyName("end_line")]
    public int EndLine { get; set; }
}

public class StructMsg
{
    [JsonPropertyName("type")]
    public string Type => "struct";

    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("path")]
    public string Path { get; set; } = "";

    [JsonPropertyName("start")]
    public int Start { get; set; }

    [JsonPropertyName("end")]
    public int End { get; set; }

    [JsonPropertyName("start_line")]
    public int StartLine { get; set; }

    [JsonPropertyName("end_line")]
    public int EndLine { get; set; }
}

public class FuncMsg
{
    [JsonPropertyName("type")]
    public string Type => "function";

    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    [JsonPropertyName("parent")]
    public string Parent { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    [JsonPropertyName("params")]
    public List<string> Params { get; set; } = new();

    [JsonPropertyName("file")]
    public string File { get; set; } = "";

    [JsonPropertyName("path")]
    public string Path { get; set; } = "";

    [JsonPropertyName("start")]
    public int Start { get; set; }

    [JsonPropertyName("end")]
    public int End { get; set; }

    [JsonPropertyName("start_line")]
    public int StartLine { get; set; }

    [JsonPropertyName("end_line")]
    public int EndLine { get; set; }
}

public class UnresolvedMsg
{
    [JsonPropertyName("type")]
    public string Type => "unresolved";

    [JsonPropertyName("fqn")]
    public string Fqn { get; set; } = "";

    [JsonPropertyName("category")]
    public string Category { get; set; } = "";
}

public class EdgeMsg
{
    [JsonPropertyName("type")]
    public string Type { get; set; } = ""; // contains | calls | uses | unresolved_call | unresolved_use

    [JsonPropertyName("from")]
    public string From { get; set; } = "";

    [JsonPropertyName("to")]
    public string To { get; set; } = "";

    [JsonPropertyName("target_type")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? TargetType { get; set; }
}
