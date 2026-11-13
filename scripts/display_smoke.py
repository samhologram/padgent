#!/usr/bin/env python3
"""Local end-to-end smoke check for Padgent's shared display HTTP/MCP API.

Run against an isolated demo instance, for example:
  uv run --with 'mcp==2.3.0' scripts/display_smoke.py
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import importlib.metadata
import json
import socket
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

from mcp import Client

GENERATED_PREVIEWS: set[str] = set()


class SmokeError(RuntimeError):
    pass


def request(base: str, path: str, *, method: str = "GET", body: Any = None, extra_headers: dict[str, str] | None = None) -> tuple[int, Any, bytes]:
    data = None if body is None else json.dumps(body).encode("utf-8")
    headers = {"Accept": "application/json"}
    headers.update(extra_headers or {})
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            raw = response.read()
            status = response.status
    except urllib.error.HTTPError as error:
        raw, status = error.read(), error.code
    try:
        parsed = json.loads(raw) if raw else None
    except (json.JSONDecodeError, UnicodeDecodeError):
        parsed = None
    return status, parsed, raw


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SmokeError(message)


def http_call(base: str, op: str, args: dict[str, Any] | None = None) -> tuple[int, Any]:
    status, value, _ = request(base, f"/v1/display/{op}", method="POST", body=args or {})
    return status, value


def mcp_post(base: str, body: dict[str, Any], protocol_version: str | None = "2025-11-25") -> tuple[int, Any]:
    headers = {"Accept": "application/json, text/event-stream", "Content-Type": "application/json"}
    if protocol_version:
        headers["MCP-Protocol-Version"] = protocol_version
    status, value, _ = request(base, "/mcp", method="POST", body=body, extra_headers=headers)
    return status, value


def raw_post(base: str, path: str, body: bytes, *, declared_length: int | None = None, extra_headers: dict[str, str] | None = None, half_close: bool = False) -> tuple[int, bytes]:
    parsed = urllib.parse.urlsplit(base)
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    headers = {"Host": f"{host}:{port}", "Connection": "close", "Content-Length": str(len(body) if declared_length is None else declared_length)}
    headers.update(extra_headers or {})
    head = f"POST {path} HTTP/1.1\r\n" + "".join(f"{name}: {value}\r\n" for name, value in headers.items()) + "\r\n"
    with socket.create_connection((host, port), timeout=5) as sock:
        sock.sendall(head.encode("ascii") + body)
        if half_close:
            sock.shutdown(socket.SHUT_WR)
        response = bytearray()
        while True:
            chunk = sock.recv(8192)
            if not chunk:
                break
            response.extend(chunk)
    status_line = bytes(response).split(b"\r\n", 1)[0].decode("ascii", errors="replace")
    parts = status_line.split()
    require(len(parts) >= 2 and parts[0].startswith("HTTP/"), "raw HTTP response had no status line")
    return int(parts[1]), bytes(response).partition(b"\r\n\r\n")[2]


def preview(base: str, key: int, time_value: int, output: Path, label: str) -> bytes:
    query = urllib.parse.urlencode({"time": time_value})
    status, _, raw = request(base, f"/v1/display/keys/{key}/preview.png?{query}")
    require(status == 200 and raw.startswith(b"\x89PNG\r\n\x1a\n"), f"key {key} preview at t={time_value} did not return PNG")
    output.mkdir(parents=True, exist_ok=True)
    filename = f"{label}-key{key}-t{time_value}.png"
    (output / filename).write_bytes(raw)
    GENERATED_PREVIEWS.add(filename)
    return raw


def assert_animated_pair(base: str, key: int, output: Path, label: str) -> str:
    first = preview(base, key, 0, output, label)
    second = preview(base, key, 2, output, label)
    h1, h2 = hashlib.sha256(first).hexdigest(), hashlib.sha256(second).hexdigest()
    require(h1 != h2, f"key {key} animation rendered identical frames at t=0 and t=2")
    return h1[:12] + "/" + h2[:12]


async def run_mcp(base: str, output: Path) -> tuple[list[str], str, str, str]:
    mcp_token: str | None = None
    async with Client(base + "/mcp") as client:
        try:
            listed = await client.list_tools()
            negotiated_protocol = client.protocol_version
            names = sorted(tool.name for tool in listed.tools)
            required = {"display_state", "display_claim", "display_set", "display_clear", "display_renew", "display_release", "display_events"}
            require(required.issubset(names), "MCP tools/list did not expose all six display tools")

            claim = await client.call_tool("display_claim", {"owner": "smoke-mcp", "keys": [2, 3], "ttl_seconds": 60})
            require(not claim.is_error, "MCP claim failed")
            claim_data = claim.structured_content
            require(isinstance(claim_data, dict) and isinstance(claim_data.get("token"), str), "MCP claim omitted structured token")
            mcp_token = claim_data["token"]

            native = {"kind": "native", "title": "MCP native", "text": "Thinking", "activity": "thinking", "hue": 220, "style": "orb"}
            result = await client.call_tool("display_set", {"token": mcp_token, "keys": {"2": native}})
            require(not result.is_error, "MCP native scene set failed")
            native_hashes = assert_animated_pair(base, 2, output, "mcp-native")

            custom = {
                "kind": "custom", "title": "MCP custom", "text": "Gradient",
                "background": "#10131e", "foreground": "#ffffff", "accent": "#4e9ce6",
                "animation": "gradient", "period_seconds": 3,
                "layers": [{"kind": "text", "x": 0.1, "y": 0.7, "width": 0.8, "height": 0.2, "size": 0.1, "color": "#ffffff", "text": "MCP"}],
            }
            result = await client.call_tool("display_set", {"token": mcp_token, "keys": {"3": custom}})
            require(not result.is_error, "MCP custom scene set failed")
            custom_hashes = assert_animated_pair(base, 3, output, "mcp-custom")

            status, _ = http_call(base, "claim", {"owner": "smoke-overlap", "keys": [2], "ttl_seconds": 10})
            require(status == 409, "overlapping HTTP claim did not fail with 409 while MCP lease was held")
            released = await client.call_tool("display_release", {"token": mcp_token})
            require(not released.is_error, "MCP release failed")
            mcp_token = None
            return names, negotiated_protocol, native_hashes, custom_hashes
        finally:
            if mcp_token:
                try:
                    await client.call_tool("display_release", {"token": mcp_token})
                except Exception:
                    pass


async def run_mcp_clear(base: str, token: str) -> None:
    async with Client(base + "/mcp") as client:
        result = await client.call_tool("display_clear", {"token": token})
        require(not result.is_error, "MCP full-lease clear failed using the HTTP-issued token")
        state = await client.call_tool("display_state", {})
        require(not state.is_error and isinstance(state.structured_content, dict), "MCP state failed after clear")
        leases = state.structured_content.get("leases", [])
        lease = next((item for item in leases if item.get("owner") == "smoke-all-keys"), None)
        require(lease is not None and lease.get("keys") == list(range(9)), "MCP clear removed or changed the HTTP-created nine-key lease")


def run(args: argparse.Namespace) -> dict[str, Any]:
    GENERATED_PREVIEWS.clear()
    base = args.url.rstrip("/")
    output = Path(args.output_dir)
    http_token: str | None = None
    mcp_names: list[str] = []
    try:
        status, health, _ = request(base, "/health")
        require(status == 200 and isinstance(health, dict) and health.get("ok") is True, "demo server health check failed")

        status, _, _ = request(base, "/health", extra_headers={"Origin": "http://example.com"})
        require(status == 403, "invalid Origin was not rejected")
        status, _, _ = request(base, "/health", extra_headers={"Host": "example.com"})
        require(status == 403, "invalid Host was not rejected")
        status, _, _ = request(base, "/v1/display/keys/0/preview.png?time=nan")
        require(status == 400, "invalid preview time was not rejected")
        status, _ = raw_post(base, "/mcp", b"{}", declared_length=20, extra_headers={"Accept": "application/json, text/event-stream", "Content-Type": "application/json", "MCP-Protocol-Version": "2025-11-25"}, half_close=True)
        require(status == 400, "truncated HTTP request body was not rejected")
        status, error_body = raw_post(base, "/mcp", b"{\"jsonrpc\":", extra_headers={"Accept": "application/json, text/event-stream", "Content-Type": "application/json", "MCP-Protocol-Version": "2025-11-25"})
        try:
            parse_error = json.loads(error_body)
        except json.JSONDecodeError as exc:
            raise SmokeError("malformed MCP JSON did not return a JSON-RPC error") from exc
        require(status == 200 and parse_error.get("error", {}).get("code") == -32700, "malformed MCP JSON did not produce parse error -32700")

        status, init = mcp_post(base, {"jsonrpc": "2.0", "id": "smoke-init", "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "display-smoke", "version": "1"},
        }}, None)
        require(status == 200 and init.get("result", {}).get("protocolVersion") == "2025-11-25", "MCP initialize failed")
        status, _, = mcp_post(base, {"jsonrpc": "2.0", "method": "notifications/initialized"})
        require(status == 202, "MCP initialized notification was not accepted")
        status, _ = mcp_post(base, {"jsonrpc": "2.0", "id": "missing-version", "method": "tools/list"}, None)
        require(status == 400, "post-initialize MCP request without version header was not rejected")

        status, claimed = http_call(base, "claim", {"owner": "smoke-http", "keys": [0, 1], "ttl_seconds": 60})
        require(status == 200 and isinstance(claimed, dict) and isinstance(claimed.get("token"), str), "HTTP claim failed")
        http_token = claimed["token"]
        status, _ = http_call(base, "set", {"token": http_token, "keys": {
            "0": {"kind": "native", "title": "HTTP native", "text": "Thinking", "activity": "thinking", "color": "#17191b", "style": "dots"},
            "1": {"kind": "custom", "title": "HTTP custom", "text": "Gradient", "background": "#15111f", "foreground": "#ffffff", "accent": "#d77a42", "animation": "gradient", "period_seconds": 3,
                  "layers": [{"kind": "text", "x": 0.1, "y": 0.7, "width": 0.8, "height": 0.2, "size": 0.1, "color": "#ffffff", "text": "HTTP"}]},
        }})
        require(status == 200, "HTTP native/custom scene set failed")
        http_frames = {
            "native": assert_animated_pair(base, 0, output, "http-native"),
            "custom": assert_animated_pair(base, 1, output, "http-custom"),
        }
        key0_before_clear = preview(base, 0, 0, output, "http-clear-before")
        key1_before_clear = preview(base, 1, 0, output, "http-clear-before")
        status, _ = http_call(base, "clear", {"token": http_token, "keys": [0]})
        require(status == 200, "HTTP partial clear failed")
        key0_after_clear = preview(base, 0, 0, output, "http-clear-after")
        key1_after_clear = preview(base, 1, 0, output, "http-clear-after")
        require(key0_before_clear != key0_after_clear, "clearing selected HTTP key did not change its rendered scene")
        require(key1_before_clear == key1_after_clear, "clearing key 0 changed the other owned key 1")
        status, state_after_clear = request(base, "/v1/display")[:2]
        require(status == 200 and any(item.get("owner") == "smoke-http" and item.get("keys") == [0, 1] for item in state_after_clear.get("leases", [])), "partial clear did not retain the HTTP lease")
        mcp_names, negotiated_protocol, mcp_native_frames, mcp_custom_frames = asyncio.run(run_mcp(base, output))

        # The MCP lease overlap was checked while the official SDK session was open.
        status, invalid = http_call(base, "claim", {"owner": "smoke-invalid", "keys": [9]})
        require(status == 400, "out-of-range key claim did not fail with 400")

        # State is intentionally token-free; then all nine keys can be claimed together.
        status, state = request(base, "/v1/display")[:2]
        require(status == 200 and isinstance(state, dict), "display state failed")
        require(all("token" not in lease for lease in state.get("leases", [])), "display state leaked lease token")

        status, _ = http_call(base, "release", {"token": http_token})
        require(status == 200, "HTTP release failed")
        http_token = None

        # MCP lease is released in run_mcp's client context; now prove full-key claim.
        status, full = http_call(base, "claim", {"owner": "smoke-all-keys", "keys": list(range(9)), "ttl_seconds": 10})
        require(status == 200 and isinstance(full, dict) and isinstance(full.get("token"), str), "claiming all nine released keys failed")
        full_token = full["token"]
        try:
            all_scenes = {
                str(key): ({"kind": "native", "title": f"Key {key}", "activity": "thinking", "hue": key * 35, "style": "orb"}
                           if key % 2 == 0 else
                           {"kind": "custom", "title": f"Key {key}", "text": f"Full keypad {key}", "animation": "gradient", "period_seconds": 3,
                            "background": "#10131e", "foreground": "#ffffff", "accent": "#4e9ce6"})
                for key in range(9)
            }
            status, _ = http_call(base, "set", {"token": full["token"], "keys": all_scenes})
            require(status == 200, "setting scenes across all nine keys failed")
            for key in range(9):
                png = preview(base, key, 0, output, "all-keys")
                require(bool(png), f"full-key preview for key {key} was empty")
            before_clear = preview(base, 0, 0, output, "all-keys-before-clear")
            asyncio.run(run_mcp_clear(base, full_token))
            after_clear = preview(base, 0, 0, output, "all-keys-after-clear")
            require(before_clear != after_clear, "MCP full-lease clear did not change the rendered key")
            status, _ = http_call(base, "release", {"token": full_token})
            require(status == 200, "all-key release failed")
            full_token = ""
        finally:
            if full_token:
                http_call(base, "release", {"token": full_token})

        status, expiring = http_call(base, "claim", {"owner": "smoke-expiry", "keys": [0], "ttl_seconds": 1})
        require(status == 200 and isinstance(expiring, dict) and isinstance(expiring.get("token"), str), "one-second lease claim failed")
        time.sleep(1.2)
        status, _ = http_call(base, "renew", {"token": expiring["token"], "ttl_seconds": 10})
        require(status == 403, "expired lease could still be renewed")
        status, replacement = http_call(base, "claim", {"owner": "smoke-expiry-reclaim", "keys": [0], "ttl_seconds": 10})
        require(status == 200 and isinstance(replacement, dict) and isinstance(replacement.get("token"), str), "expired lease did not free its key")
        http_call(base, "release", {"token": replacement["token"]})

        return {
            "ok": True,
            "negotiated_protocol": negotiated_protocol,
            "server_protocol": health.get("displayApi", {}).get("mcp", {}).get("protocolVersion"),
            "mcp_sdk_version": importlib.metadata.version("mcp"),
            "mcp_tools": mcp_names,
            "checks": ["Origin and Host rejection", "invalid preview time", "truncated HTTP body and malformed MCP JSON rejection", "MCP version negotiation/header enforcement", "http native/custom scenes", "HTTP partial clear preserves other key and lease", "official MCP SDK list/call and HTTP-token full clear", "animated preview frames", "cross-transport lease conflict", "invalid key rejection", "token-free state", "all nine key scenes/previews", "lease expiry and key reclaim"],
            "frame_sha256_prefixes": {"http_native": http_frames["native"], "http_custom": http_frames["custom"], "mcp_native": mcp_native_frames, "mcp_custom": mcp_custom_frames},
            "preview_files": sorted(GENERATED_PREVIEWS),
            "preview_count": len(GENERATED_PREVIEWS),
        }
    finally:
        if http_token:
            http_call(base, "release", {"token": http_token})


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:47801", help="isolated demo server base URL")
    parser.add_argument("--output-dir", default="/private/tmp/padgent-display-smoke", help="directory for generated public demo previews")
    parser.add_argument("--summary-file", default="/private/tmp/padgent-display-smoke-results.json", help="where to save the token-free JSON result summary")
    args = parser.parse_args()
    try:
        summary = run(args)
    except Exception as exc:
        print(json.dumps({"ok": False, "error": str(exc)}, indent=2))
        raise SystemExit(1) from exc
    summary_path = Path(args.summary_file)
    summary_path.parent.mkdir(parents=True, exist_ok=True)
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
