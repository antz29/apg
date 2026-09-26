#pragma once

#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

// ── FQN / scope arithmetic + name resolution (extracted from main.cpp) ─

static std::string clean_fqn(const std::string &raw) {
    std::string out;
    for (char c : raw) {
        if (c == ',') continue;
        if (c == '\n' || c == '\r') continue;
        if (c == '"') continue;
        out += c;
    }
    while (!out.empty() && out.back() == '.') out.pop_back();
    while (!out.empty() && out.front() == '.') out.erase(out.begin());
    size_t i = 0;
    while (i + 1 < out.size()) {
        if (out[i] == '.' && out[i+1] == '.')
            out.erase(i, 1);
        else
            i++;
    }
    return out;
}

static std::string fqn_in_scope(const std::string &name, const std::vector<std::string> &scope) {
    if (scope.empty()) return name;
    std::string out;
    for (size_t i = 0; i < scope.size(); i++) {
        if (i > 0) out += ".";
        out += scope[i];
    }
    out += "." + name;
    return out;
}

static std::string resolve_type_fqn(const std::string &type_name,
    const std::vector<std::string> &scope,
    const std::unordered_set<std::string> &fqn_set)
{
    if (type_name.empty()) return "";
    if (fqn_set.count(type_name)) return type_name;
    std::string scoped = fqn_in_scope(type_name, scope);
    if (fqn_set.count(scoped)) return scoped;
    return "";
}

static std::string join_params(const std::vector<std::string> &params) {
    std::string out;
    for (size_t i = 0; i < params.size(); i++) {
        if (i > 0) out += ",";
        out += params[i];
    }
    return out;
}

// The enclosing-scope FQN of a declaration (everything before the last dot),
// or "" if there is none.
static std::string parent_of(const std::string &fqn) {
    size_t dot = fqn.rfind('.');
    if (dot == std::string::npos) return "";
    return fqn.substr(0, dot);
}

static std::string resolve_name(const std::string &name,
    const std::vector<std::string> &scope,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::unordered_set<std::string> &fqn_set)
{
    auto it = name_map.find(name);
    if (it == name_map.end()) return "";
    const auto &candidates = it->second;

    for (int i = (int)scope.size(); i >= 0; i--) {
        std::string prefix;
        for (int j = 0; j < i; j++) {
            if (j > 0) prefix += ".";
            prefix += scope[j];
        }
        for (const auto &candidate : candidates) {
            if (prefix.empty()) {
                if (candidate.find('.') == std::string::npos || candidate == name) {
                    return candidate;
                }
            } else {
                std::string expected = prefix + "." + name;
                if (candidate == expected) return candidate;
            }
        }
    }
    return "";
}
