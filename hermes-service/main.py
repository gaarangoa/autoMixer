"""automixer hermes-service — the bridge sidecar between AutoMixer (Tauri/Rust)
and the Hermes agent.

It holds one persistent `hermes acp` connection (ACP = Agent Client Protocol,
line-delimited JSON-RPC over stdio) and exposes a small HTTP/SSE surface that the
Rust backend drives exactly like the audio sidecar:

  GET  /health              -> readiness
  POST /chat {sessionId, userText}  -> Server-Sent Events stream of:
        {"type":"chunk",  "text": "..."}     agent message tokens
        {"type":"thought","text": "..."}     reasoning tokens
        {"type":"tool",   "name": "...", "status": "...", "kind": "..."}
        {"type":"done",   "stopReason": "end_turn"}
        {"type":"error",  "message": "..."}

Hermes' tool calls go through the `automixer-mcp` server (handed in per ACP
session) to the app's in-process control surface, which mutates the live session
and refreshes the UI. We auto-approve ONLY automixer tools; everything else is
refused, so the embedded agent can't touch the shell/filesystem.
"""

from __future__ import annotations

import asyncio
import json
import os
from contextlib import asynccontextmanager
from pathlib import Path

import acp
from acp.schema import (
    AllowedOutcome,
    DeniedOutcome,
    McpServerStdio,
    RequestPermissionResponse,
)
from fastapi import FastAPI
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

HERMES = os.environ.get(
    "AUTOMIXER_HERMES_BIN",
    str(Path.home() / ".hermes" / "hermes-agent" / "venv" / "bin" / "hermes"),
)
UV = os.environ.get("AUTOMIXER_UV", str(Path.home() / ".local" / "bin" / "uv"))
MCP_DIR = os.environ.get("AUTOMIXER_MCP_DIR", str(Path(__file__).parent / "automixer-mcp"))

# Tool-name fragments we trust (our control-surface tools). Anything else the
# agent tries to call is refused.
ALLOWED_TOOL_HINTS = ("automixer", "get_session", "set_track", "adjust_track", "mute_track",
                      "solo_track", "set_eq", "set_compressor", "set_high_pass", "set_low_pass",
                      "set_reverb", "set_delay", "set_master", "undo", "redo")


def _tool_allowed(tool_call) -> bool:
    label = " ".join(
        str(getattr(tool_call, attr, "") or "")
        for attr in ("title", "tool_call_id", "kind")
    ).lower()
    return any(hint in label for hint in ALLOWED_TOOL_HINTS)


class Bridge:
    """One persistent `hermes acp` connection; routes streaming updates per ACP
    session to the in-flight request's queue."""

    def __init__(self) -> None:
        self.conn = None
        self.proc = None
        self._cm = None
        self.queues: dict[str, asyncio.Queue] = {}      # acp_session_id -> event queue
        self.acp_for_mix: dict[str, str] = {}           # automixer session id -> acp session id
        self.tool_names: dict[str, str] = {}            # tool_call_id -> friendly name
        self.turn_lock = asyncio.Lock()                 # one prompt at a time (single chat UI)

    @staticmethod
    def _clean_tool(name: str) -> str:
        name = str(name or "")
        for prefix in ("mcp_automixer_", "automixer_", "mcp_"):
            if name.startswith(prefix):
                return name[len(prefix):]
        return name

    # ---- ACP Client callbacks (the agent talks back to us through these) ----
    async def session_update(self, session_id, update, **_) -> None:
        queue = self.queues.get(session_id)
        if queue is None:
            return
        kind = type(update).__name__
        if kind == "AgentMessageChunk":
            text = getattr(getattr(update, "content", None), "text", None)
            if text:
                await queue.put({"type": "chunk", "text": text})
        elif kind == "AgentThoughtChunk":
            text = getattr(getattr(update, "content", None), "text", None)
            if text:
                await queue.put({"type": "thought", "text": text})
        elif kind in ("ToolCallStart", "ToolCallProgress"):
            tcid = getattr(update, "tool_call_id", None)
            if kind == "ToolCallStart":
                name = self._clean_tool(getattr(update, "title", None) or tcid or "")
                if tcid:
                    self.tool_names[tcid] = name
                status = "start"
            else:
                name = self.tool_names.get(tcid, self._clean_tool(getattr(update, "title", None) or ""))
                status = str(getattr(update, "status", "") or "")
            await queue.put({"type": "tool", "name": name, "status": status, "kind": kind})

    async def request_permission(self, options, session_id, tool_call, **_):
        if _tool_allowed(tool_call):
            chosen = next((o for o in options if "allow" in str(getattr(o, "kind", "")).lower()), options[0])
            oid = getattr(chosen, "option_id", None) or getattr(chosen, "optionId", None)
            return RequestPermissionResponse(outcome=AllowedOutcome(outcome="selected", option_id=oid))
        # Refuse anything that isn't one of our control-surface tools.
        return RequestPermissionResponse(outcome=DeniedOutcome(outcome="cancelled"))

    # ---- stubs: the embedded agent must not touch fs/terminal ----
    async def read_text_file(self, *a, **k):
        raise acp.RequestError(code=-32601, message="fs disabled")

    async def write_text_file(self, *a, **k):
        raise acp.RequestError(code=-32601, message="fs disabled")

    async def create_terminal(self, *a, **k):
        raise acp.RequestError(code=-32601, message="terminal disabled")

    async def ext_method(self, method, params):
        return {}

    async def ext_notification(self, method, params):
        return None

    # ---- lifecycle ----
    async def start(self) -> None:
        env = {"HOME": os.environ["HOME"], "PATH": os.environ.get("PATH", "")}
        if "HERMES_HOME" in os.environ:
            env["HERMES_HOME"] = os.environ["HERMES_HOME"]
        self._cm = acp.spawn_agent_process(self, HERMES, "acp", env=env)
        self.conn, self.proc = await self._cm.__aenter__()
        await self.conn.initialize(
            protocol_version=acp.PROTOCOL_VERSION,
            client_capabilities={"fs": {"readTextFile": False, "writeTextFile": False}},
        )

    async def stop(self) -> None:
        if self._cm is not None:
            try:
                await self._cm.__aexit__(None, None, None)
            except Exception:
                pass

    async def acp_session_for(self, mix_session_id: str) -> str:
        """Reuse one ACP session per AutoMixer session so Hermes' memory accrues
        per conversation. The automixer MCP server is the only toolset attached."""
        existing = self.acp_for_mix.get(mix_session_id)
        if existing:
            return existing
        # Rely on the globally-configured `automixer` MCP server (in ~/.hermes/config.yaml)
        # rather than passing one per-session: only the config entry can carry a per-server
        # `timeout` (ACP's McpServerStdio has no timeout field), and the long-running
        # edit_video / auto_mix tools need more than the default 300s.
        new = await self.conn.new_session(cwd=MCP_DIR, mcp_servers=None)
        self.acp_for_mix[mix_session_id] = new.session_id
        return new.session_id


bridge = Bridge()


@asynccontextmanager
async def lifespan(app: FastAPI):
    await bridge.start()
    try:
        yield
    finally:
        await bridge.stop()


app = FastAPI(lifespan=lifespan)


@app.get("/health")
async def health():
    return {"ok": True, "service": "automixer-hermes"}


class ResetBody(BaseModel):
    sessionId: str


@app.post("/reset")
async def reset(body: ResetBody):
    """Forget the conversation for this AutoMixer session. Drops the ACP session
    mapping so the next /chat starts a brand-new session with no prior context — used
    by the UI's "Clear chat" so stale instructions (e.g. an earlier look preset) don't
    leak into a fresh request."""
    old = bridge.acp_for_mix.pop(body.sessionId, None)
    if old is not None:
        bridge.queues.pop(old, None)
        try:
            await bridge.conn.cancel(session_id=old)
        except Exception:
            pass
    return {"ok": True, "cleared": old is not None}


class ChatBody(BaseModel):
    sessionId: str
    userText: str


@app.post("/chat")
async def chat(body: ChatBody):
    async def gen():
        # Serialize turns — the chat UI runs one conversation at a time.
        async with bridge.turn_lock:
            acp_sid = await bridge.acp_session_for(body.sessionId)
            queue: asyncio.Queue = asyncio.Queue()
            bridge.queues[acp_sid] = queue

            async def run_prompt():
                try:
                    prompt = (
                        f"The current AutoMixer session id is {body.sessionId}. "
                        f"Use the automixer tools to inspect and adjust this session "
                        f"(call get_session first to see the tracks). "
                        f"User request: {body.userText}"
                    )
                    resp = await bridge.conn.prompt(prompt=[acp.text_block(prompt)], session_id=acp_sid)
                    await queue.put({"type": "done", "stopReason": getattr(resp, "stop_reason", "end_turn")})
                except Exception as exc:  # noqa: BLE001
                    await queue.put({"type": "error", "message": str(exc)})
                finally:
                    await queue.put(None)

            task = asyncio.create_task(run_prompt())
            try:
                while True:
                    event = await queue.get()
                    if event is None:
                        break
                    yield f"data: {json.dumps(event)}\n\n"
            except asyncio.CancelledError:
                # The Rust client dropped the connection (user pressed Stop) —
                # cancel the in-flight ACP turn so the agent actually stops.
                try:
                    await bridge.conn.cancel(session_id=acp_sid)
                except Exception:
                    pass
                raise
            finally:
                bridge.queues.pop(acp_sid, None)
                if not task.done():
                    task.cancel()
                try:
                    await task
                except Exception:
                    pass

    return StreamingResponse(gen(), media_type="text/event-stream")
