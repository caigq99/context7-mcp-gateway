#!/usr/bin/env python3
"""Exercise the documented upstream or a local gateway without printing secrets."""

import argparse
import getpass
import json
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = build_opener(NoRedirect(), ProxyHandler({}))
QUESTION = "How do I create a GET route with Axum Router?"
MAX_BODY = 2 * 1024 * 1024


def request(url, key, payload=None, extra_headers=None):
    headers = {"Authorization": f"Bearer {key}", "Accept": "application/json"}
    if payload is not None:
        headers.update({"Content-Type": "application/json", "Accept": "application/json, text/event-stream"})
    headers.update(extra_headers or {})
    data = None if payload is None else json.dumps(payload).encode()
    try:
        response = OPENER.open(Request(url, data=data, headers=headers), timeout=25)
    except HTTPError as error:
        response = error
    with response:
        content_type = response.headers.get_content_type()
        body = None
        if content_type == "text/event-stream":
            remaining = MAX_BODY
            data_lines = []
            for line in iter(lambda: response.readline(remaining + 1), b""):
                remaining -= len(line)
                if remaining <= 0:
                    break
                if line.startswith(b"data:"):
                    data_lines.append(line[5:].strip())
                if not line.strip() and data_lines:
                    candidate = json.loads(b"\n".join(data_lines))
                    data_lines = []
                    if candidate.get("id") == (payload or {}).get("id"):
                        body = candidate
                        break
        elif content_type == "application/json":
            raw = response.read(MAX_BODY + 1)
            if len(raw) <= MAX_BODY and raw:
                body = json.loads(raw)
        response_headers = response.headers
        return response.status, response_headers, body


def mcp_check(url, key):
    client = {"name": "context7-gateway-smoke", "version": "1.0.0"}
    modern = "2026-07-28"

    def modern_call(rpc_method, params, message_id):
        headers = {"MCP-Protocol-Version": modern, "Mcp-Method": rpc_method}
        if rpc_method == "tools/call":
            headers["Mcp-Name"] = params["name"]
        meta = {"io.modelcontextprotocol/protocolVersion": modern,
                "io.modelcontextprotocol/clientInfo": client,
                "io.modelcontextprotocol/clientCapabilities": {}}
        return request(url, key, {"jsonrpc": "2.0", "id": message_id, "method": rpc_method,
                                  "params": {**params, "_meta": meta}}, headers)

    tool_status, tool_headers, listed = modern_call("tools/list", {}, 1)
    print(f"MCP {modern} tools/list: HTTP {tool_status} ({tool_headers.get_content_type()})")
    if tool_status == 200 and isinstance((listed or {}).get("result", {}).get("tools"), list):
        modern_supported = True
    elif tool_status in {400, 404}:
        modern_supported = False
    else:
        return False

    version = "2025-11-25"
    init_status, init_headers, initial = request(url, key, {"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": version, "capabilities": {}, "clientInfo": client}})
    initial_result = (initial or {}).get("result", {})
    negotiated = initial_result.get("protocolVersion")
    legacy_session = init_headers.get("Mcp-Session-Id")
    print(f"MCP initialize: HTTP {init_status}, negotiated={negotiated or 'none'}, session={'yes' if legacy_session else 'no'}")

    if init_status == 200 and isinstance(negotiated, str):
        client_headers = {"MCP-Protocol-Version": negotiated}
        if legacy_session:
            client_headers["Mcp-Session-Id"] = legacy_session
        status, _, _ = request(url, key, {"jsonrpc": "2.0", "method": "notifications/initialized"}, client_headers)
        print(f"MCP notifications/initialized: HTTP {status}")
        if status != 202:
            return False

        def call(rpc_method, params, message_id):
            return request(url, key, {"jsonrpc": "2.0", "id": message_id, "method": rpc_method,
                                      "params": params}, client_headers)

        status, headers, listed = call("tools/list", {}, 2)
        print(f"MCP {negotiated} tools/list: HTTP {status} ({headers.get_content_type()})")
        if status != 200:
            return False
    elif modern_supported:
        call = modern_call
    else:
        return False

    available = {item.get("name") for item in (listed or {}).get("result", {}).get("tools", [])}
    tools = {"resolve-library-id": {"libraryName": "axum", "query": QUESTION},
             "query-docs": {"libraryId": "/tokio-rs/axum", "query": QUESTION, "language": "Rust"}}
    for message_id, (name, arguments) in enumerate(tools.items(), start=3):
        if name not in available:
            print(f"MCP tools/list does not include {name}")
            return False
        status, headers, body = call("tools/call", {"name": name, "arguments": arguments}, message_id)
        result = (body or {}).get("result")
        valid = isinstance(result, dict) and not result.get("isError", False)
        print(f"MCP {name}: HTTP {status} ({headers.get_content_type()}), result={'ok' if valid else 'error'}")
        if status != 200 or not valid:
            return False
    return True


def rest_check(origin, prefix, key, upstream):
    if upstream:
        status, _, body = request(f"{origin}/api/v2/usage?days=1", key)
        valid = isinstance(body, dict) and "member" in body and "teamspace" in body
        print(f"REST usage: HTTP {status}, shape={'ok' if valid else 'unexpected'}")
        if status != 200 or not valid:
            return False

    operations = [
        ("v2/libs/search", {"libraryName": "axum", "query": QUESTION, "fast": "true"}, "results"),
        ("v2/context", {"libraryId": "/tokio-rs/axum", "query": QUESTION, "type": "json"}, "codeSnippets"),
        ("v3/search", {"library": "axum", "query": QUESTION, "type": "json"}, "codeSnippets"),
        ("v3/fetch", {"url": "https://docs.rs/axum/latest/axum/", "tokens": 500, "type": "json"}, "url"),
    ]
    ok = True
    for path, params, field in operations:
        status, headers, body = request(f"{origin}{prefix}/{path}?{urlencode(params)}", key)
        valid = isinstance(body, dict) and field in body
        print(f"REST {path}: HTTP {status}, shape={'ok' if valid else 'unexpected'}, "
              f"rate-limit headers={'yes' if 'RateLimit-Limit' in headers else 'no'}")
        ok &= status == 200 and valid
        if status == 429:
            break
    return ok


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gateway-origin", help="Loopback HTTP or HTTPS origin of the gateway")
    args = parser.parse_args()
    if args.gateway_origin:
        parsed = urlsplit(args.gateway_origin)
        if (parsed.scheme != "https" and (parsed.scheme != "http" or parsed.hostname not in {"localhost", "127.0.0.1", "::1"})
                or not parsed.hostname or parsed.username or parsed.password or parsed.path not in {"", "/"}
                or parsed.query or parsed.fragment):
            parser.error("provide an HTTPS or local HTTP origin without credentials or a path")
    key = getpass.getpass("Gateway Key: " if args.gateway_origin else "Context7 test Key: ").strip()
    if not key:
        parser.error("a key is required")
    if args.gateway_origin:
        if not key.startswith("c7g_"):
            parser.error("the local gateway test expects a Gateway Key")
        origin = args.gateway_origin.rstrip("/")
        mcp_url, rest_origin, prefix = f"{origin}/mcp", origin, "/api/context7"
    else:
        if not key.startswith("ctx7sk-"):
            parser.error("the direct test expects a Context7 API Key")
        mcp_url, rest_origin, prefix = "https://mcp.context7.com/mcp", "https://context7.com", "/api"
    try:
        if not rest_check(rest_origin, prefix, key, not args.gateway_origin):
            return 1
        return 0 if mcp_check(mcp_url, key) else 1
    except (URLError, TimeoutError, OSError, ValueError) as err:
        print(f"Transport or response error: {type(err).__name__}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
