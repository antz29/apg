#!/usr/bin/env python3
"""Thin MCP server that wraps `lbug` CLI for stdin/stdout JSON-RPC."""

import json
import subprocess
import sys


def run_query(db_path: str, query: str, max_rows: int = 1024) -> str:
    result = subprocess.run(
        ["lbug", db_path, "-readonly", "-csv", "-separator", ","],
        input=query,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip())
    output = result.stdout.strip()
    lines = output.splitlines()
    if len(lines) > max_rows + 1:
        header = lines[0]
        body = lines[1 : max_rows + 1]
        truncated = f"\n...truncated {len(lines) - max_rows - 1} rows"
        output = "\n".join([header] + body + [truncated])
    return output


def handle_request(db_path: str, req: dict) -> dict | None:
    method = req.get("method", "")
    req_id = req.get("id")

    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "ladybug-mcp", "version": "0.1.0"},
            },
        }
    elif method == "notifications/initialized":
        return None
    elif method == "tools/list":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "tools": [
                    {
                        "name": "query",
                        "description": "Execute a read-only Cypher query on the LadybugDB graph database",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": {
                                    "type": "string",
                                    "description": "Cypher query to execute",
                                }
                            },
                            "required": ["query"],
                        },
                    }
                ]
            },
        }
    elif method == "tools/call":
        tool_name = req["params"]["name"]
        args = req["params"].get("arguments", {})
        if tool_name == "query":
            try:
                result = run_query(db_path, args["query"])
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [{"type": "text", "text": result}],
                        "isError": False,
                    },
                }
            except Exception as e:
                return {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "content": [{"type": "text", "text": str(e)}],
                        "isError": True,
                    },
                }
    return None


def main():
    if len(sys.argv) < 2:
        print("Usage: ladybug_mcp.py <db-path>", file=sys.stderr)
        sys.exit(1)
    db_path = sys.argv[1]

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        resp = handle_request(db_path, req)
        if resp is not None:
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
