#!/usr/bin/env python3
"""Real newline JSON-RPC peer for MCPort's transport integration tests."""
import json
import sys
import time

legacy = "--legacy" in sys.argv
sequence = 0
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    params = request.get("params", {})
    result = {"resultType": "complete"}
    if method == "server/discover":
        if legacy:
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "legacy"}}), flush=True)
            continue
        result.update(supportedVersions=["2026-07-28"], capabilities={"tools": {}, "resources": {}, "prompts": {}}, ttlMs=0, cacheScope="private")
    elif method == "initialize":
        result = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}, "resources": {}, "prompts": {}}, "serverInfo": {"name": "fixture", "version": "1"}}
    elif method == "tools/list":
        if "--paged" in sys.argv and not params.get("cursor"):
            result.update(tools=[], nextCursor="second")
        elif "--cursor-loop" in sys.argv:
            result.update(tools=[], nextCursor="loop")
        else:
            result.update(tools=[{"name": "echo", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}, "outputSchema": {"type": "object", "required": ["echoed", "sequence"], "properties": {"echoed": {"type": "boolean"}, "sequence": {"type": "integer"}}}}])
    elif method == "tools/call":
        if params["name"] == "needs_roots":
            print(json.dumps({"jsonrpc": "2.0", "id": "roots-request", "method": "roots/list", "params": {}}), flush=True)
            reply = json.loads(sys.stdin.readline())
            assert reply["error"]["code"] == -32601
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32603, "message": "Roots are required"}}), flush=True)
            continue
        sequence += 1
        if params["name"] == "slow":
            time.sleep(5)
        token = params.get("_meta", {}).get("progressToken")
        if token is not None:
            print(json.dumps({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progressToken": token, "progress": 1, "total": 1}}), flush=True)
        if params["name"] == "large":
            result.update(content=[{"type": "text", "text": "x" * 100000}])
        else:
            result.update(content=[{"type": "text", "text": params.get("arguments", {}).get("text", "hello")}, {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="}], structuredContent={"echoed": True, "sequence": sequence}, isError=params["name"] == "fail")
    elif method == "resources/list":
        result.update(resources=[{"uri": "fixture://hello", "name": "hello"}])
    elif method == "resources/read":
        result.update(contents=[{"uri": params["uri"], "text": "fixture text"}])
    elif method == "prompts/list":
        result.update(prompts=[{"name": "review"}])
    elif method == "prompts/get":
        result.update(messages=[{"role": "user", "content": {"type": "text", "text": "Please review"}}])
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
