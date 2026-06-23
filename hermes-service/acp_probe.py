"""P2 spike: drive `hermes acp` as an ACP client and prove a streamed,
tool-calling turn against the user's local model, with our automixer-mcp server
handed in per-session. Run with the Hermes venv python (has `acp` + pydantic).

    ~/.hermes/hermes-agent/venv/bin/python3 acp_probe.py <session_id> <track_name> <gain_db>
"""

import asyncio
import os
import sys
from pathlib import Path

import acp
from acp.schema import (
    AllowedOutcome,
    McpServerStdio,
    RequestPermissionResponse,
)

HERMES = str(Path.home() / ".hermes" / "hermes-agent" / "venv" / "bin" / "hermes")
UV = str(Path.home() / ".local" / "bin" / "uv")
MCP_DIR = "/Volumes/data/autoMixer/hermes-service/automixer-mcp"


class ProbeClient:
    """Minimal ACP client: stream updates, auto-allow tool calls, stub the rest."""

    def __init__(self) -> None:
        self.text_chunks: list[str] = []
        self.tool_events: list[str] = []

    async def session_update(self, session_id: str, update, **kwargs) -> None:
        kind = type(update).__name__
        if kind == "AgentMessageChunk":
            content = getattr(update, "content", None)
            text = getattr(content, "text", None)
            if text:
                self.text_chunks.append(text)
                print(text, end="", flush=True)
        elif kind == "AgentThoughtChunk":
            content = getattr(update, "content", None)
            text = getattr(content, "text", None)
            if text:
                print(f"\n  [thinking] {text}", flush=True)
        elif kind in ("ToolCallStart", "ToolCallProgress"):
            title = getattr(update, "title", None) or getattr(update, "tool_call_id", "")
            status = getattr(update, "status", "")
            raw_in = getattr(update, "raw_input", None)
            line = f"\n  [tool {kind} status={status}] {title} input={raw_in}"
            self.tool_events.append(line)
            print(line, flush=True)
        else:
            print(f"\n  [update {kind}]", flush=True)

    async def request_permission(self, options, session_id, tool_call, **kwargs):
        # Auto-allow: pick the first option whose kind grants access.
        chosen = None
        for opt in options:
            kind = getattr(opt, "kind", "") or ""
            if "allow" in str(kind):
                chosen = opt
                break
        chosen = chosen or options[0]
        oid = getattr(chosen, "option_id", None) or getattr(chosen, "optionId", None)
        title = getattr(tool_call, "title", "") or getattr(tool_call, "tool_call_id", "")
        print(f"\n  [permission -> ALLOW {oid}] for {title}", flush=True)
        return RequestPermissionResponse(outcome=AllowedOutcome(outcome="selected", option_id=oid))

    # --- stubs the agent shouldn't need when scoped to automixer tools ---
    async def read_text_file(self, *a, **k):
        raise acp.RequestError(code=-32601, message="fs disabled in probe")

    async def write_text_file(self, *a, **k):
        raise acp.RequestError(code=-32601, message="fs disabled in probe")

    async def create_terminal(self, *a, **k):
        raise acp.RequestError(code=-32601, message="terminal disabled in probe")

    async def ext_method(self, method, params):
        return {}

    async def ext_notification(self, method, params):
        return None


async def main(session_id: str, track_name: str, gain_db: float) -> int:
    env = {
        "HOME": os.environ["HOME"],
        "PATH": os.environ.get("PATH", ""),
    }
    client = ProbeClient()
    async with acp.spawn_agent_process(client, HERMES, "acp", env=env) as (conn, proc):
        await conn.initialize(
            protocol_version=acp.PROTOCOL_VERSION,
            client_capabilities={"fs": {"readTextFile": False, "writeTextFile": False}},
        )
        mcp = McpServerStdio(
            name="automixer",
            command=UV,
            args=["run", "--directory", MCP_DIR, "server.py"],
            env=[],
        )
        new = await conn.new_session(cwd=MCP_DIR, mcp_servers=[mcp])
        sid = new.session_id
        print(f"[session {sid}]", flush=True)
        prompt = (
            f"AutoMixer session id is {session_id}. Use ONLY the automixer tools. "
            f"First call get_session to list the tracks. Then call set_track_gain to set "
            f"the track named '{track_name}' to {gain_db} dB. Finally state the new gainDb."
        )
        resp = await conn.prompt(prompt=[acp.text_block(prompt)], session_id=sid)
        print(f"\n[stop_reason={getattr(resp, 'stop_reason', '?')}]", flush=True)
        print(f"[tool_events={len(client.tool_events)}]", flush=True)
    return 0


if __name__ == "__main__":
    sid = sys.argv[1] if len(sys.argv) > 1 else "940964c1-026f-40cd-a8f9-d37eeba2bfa8"
    track = sys.argv[2] if len(sys.argv) > 2 else "Worlds collide"
    gain = float(sys.argv[3]) if len(sys.argv) > 3 else -6.0
    sys.exit(asyncio.run(main(sid, track, gain)))
