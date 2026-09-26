#pragma once

#include <cstdint>
#include <cstdio>
#include <string>

// ── JSONL serialization / sink (extracted from main.cpp) ─────────────

// When non-null, scanner output is appended here instead of stdout. Used by
// the --self-test harness to compare a full scan against a target-filtered one
// in-process. Normally null, so the emitted stream is byte-identical.
static std::string *captureOut = nullptr;

static void emit_json(const std::string &json) {
    if (captureOut) {
        *captureOut += json;
        *captureOut += '\n';
        return;
    }
    printf("%s\n", json.c_str());
}

static std::string json_esc(const std::string &s) {
    std::string out;
    out.reserve(s.size() + 2);
    for (char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default: out += c;
        }
    }
    return out;
}

struct JsonBuilder {
    std::string buf;
    JsonBuilder() { buf = "{"; }
    JsonBuilder &field(const std::string &key, const std::string &val) {
        if (buf.size() > 1) buf += ",";
        buf += "\"" + key + "\":\"" + json_esc(val) + "\"";
        return *this;
    }
    JsonBuilder &field(const std::string &key, uint32_t val) {
        if (buf.size() > 1) buf += ",";
        buf += "\"" + key + "\":" + std::to_string(val);
        return *this;
    }
    // Appends a pre-built JSON value (e.g. the params array) verbatim.
    JsonBuilder &raw(const std::string &key, const std::string &val) {
        if (buf.size() > 1) buf += ",";
        buf += "\"" + key + "\":" + val;
        return *this;
    }
    std::string done() { buf += "}"; return buf; }
};
