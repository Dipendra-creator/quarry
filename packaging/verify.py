"""Check native machine types, dynamic libraries and the packaged MCP protocol."""
import argparse
import json
import os
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

from release import BIN, ROOT, TARGET, version


def machine(path, platform):
    with path.open("rb") as stream:
        header = stream.read(64)
        if platform == "windows":
            if header[:2] != b"MZ":
                raise ValueError(f"{path} is not a Windows executable")
            stream.seek(struct.unpack_from("<I", header, 60)[0])
            signature = stream.read(6)
            if signature[:4] != b"PE\0\0":
                raise ValueError("Missing PE signature")
            return {0x8664: "x64", 0xAA64: "arm64"}[struct.unpack_from("<H", signature, 4)[0]]
        if platform == "linux":
            if header[:5] != b"\x7fELF\x02":
                raise ValueError(f"{path} is not a 64-bit Linux executable")
            order = "<" if header[5] == 1 else ">"
            return {62: "x64", 183: "arm64"}[struct.unpack_from(order + "H", header, 18)[0]]
        if header[:4] != b"\xcf\xfa\xed\xfe":
            raise ValueError(f"{path} is not a native 64-bit Mach-O executable")
        return {0x01000007: "x64", 0x0100000C: "arm64"}[struct.unpack_from("<I", header, 4)[0]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=("windows", "macos", "linux"), required=True)
    parser.add_argument("--arch", choices=("x64", "arm64"), required=True)
    parser.add_argument("--binary-dir", type=Path, default=BIN)
    args = parser.parse_args()
    suffix = ".exe" if args.platform == "windows" else ""
    for name in ("quarry", "quarry-mcp"):
        binary = args.binary_dir / (name + suffix)
        actual = machine(binary, args.platform)
        if actual != args.arch:
            raise ValueError(f"{name}: requested {args.arch}, built {actual}")
        if args.platform == "linux":
            result = subprocess.run(["ldd", str(binary)], capture_output=True, text=True, check=True)
            if "not found" in result.stdout:
                raise ValueError(f"Missing runtime libraries: {result.stdout}")
            print(result.stdout)
        elif args.platform == "macos":
            libraries = subprocess.check_output(["otool", "-L", str(binary)], text=True)
            for line in libraries.splitlines()[1:]:
                dependency = line.strip().split(" (", 1)[0]
                if not dependency.startswith(("/System/Library/", "/usr/lib/")):
                    raise ValueError(f"Unbundled macOS dependency: {dependency}")
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "quarry-release-check", "version": "1.0"}}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    ]
    TARGET.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=TARGET, prefix="mcp-check-") as directory:
        if not Path(directory).resolve().is_relative_to(TARGET.resolve()):
            raise ValueError("MCP check directory escaped target")
        env = os.environ.copy()
        env["QUARRY_CONFIG_DIR"] = directory
        response = subprocess.run(
            [str(args.binary_dir / ("quarry-mcp" + suffix)), "--standalone"],
            input="".join(json.dumps(item) + "\n" for item in requests),
            capture_output=True, text=True, timeout=30, env=env, check=True,
        )
    replies = {item["id"]: item for item in map(json.loads, response.stdout.splitlines())}
    expected = {"name": "quarry", "version": version()}
    if replies[1]["result"]["serverInfo"] != expected:
        raise ValueError("MCP binary version does not match Cargo.toml")
    tools = replies[2]["result"]["tools"]
    if not {"list_connections", "select_rows", "run_sql", "explain_query"} <= {tool["name"] for tool in tools}:
        raise ValueError("MCP server is missing database tools")
    print(f"{args.platform} {args.arch}: native binaries verified; MCP {version()} lists {len(tools)} tools.")


if __name__ == "__main__":
    main()
