# P0 spike — Hermes embedding feasibility findings

Spike run against a locally-installed Hermes Agent (Nous Research, MIT, Python 3.11
via `uv`, `~/.hermes/`) and AutoMixer's in-process control surface (P1). Orchestration
model: local `qwen3.6-35b-a3b` at `http://127.0.0.1:2256` (user's endpoint). DGX Spark
available at `http://127.0.0.1:11435` for vision / heavier orchestration.

## The 5 unknowns — answered

1. **Does Hermes drive a model loop programmatically?** ✅ Yes. `hermes -z "<prompt>"`
   one-shot returned a correct reply through the local endpoint. Non-interactive,
   no TTY needed.

2. **Streaming + tool-call events transport.** ✅ **ACP** (`hermes acp`, stdio/JSON-RPC,
   "editor-native agent" for VS Code/Zed/JetBrains). `hermes acp --check` → "Hermes ACP
   check OK". ACP is the Agent Client Protocol: streaming `session/update` notifications
   carry agent-message chunks **and** structured tool-call status updates, which map
   directly onto AutoMixer's existing `llm:chunk` / tool events. **No documented REST/SSE
   server subcommand exists, so the earlier "REST-SSE preferred" ranking is reversed:
   ACP is the transport.** (Non-streaming fallback unneeded — ACP streams.)

3. **MCP tool → external HTTP control endpoint → live session.** ✅ Proven. The thin
   `automixer-mcp` stdio server (FastMCP) reads `~/.automixer/control.json` and calls the
   P1 control endpoint. Exercised directly: `get_session` read the live session, and
   `set_track_gain(-4.0)` mutated track "Worlds collide" and was restored to 0.0 — against
   the running app.

4. **Hermes loads our MCP server.** ✅ `hermes mcp add automixer --command uv --args run
   --directory <dir> server.py` → "Connected! Found 2 tool(s)" → saved/enabled (2/2).
   `hermes mcp list` shows it enabled.

5. **Per-request session context.** ✅ Tools take `session_id` as an argument (and the app
   resolves the live session), so context is injected per call without polluting Hermes'
   persistent memory.

## Tool-approval finding (important for P2)

`hermes --yolo` (global gate-off) is **not** the path — it's an ungated autonomous
shell-capable loop and was (correctly) blocked. Hermes has **per-tool enablement**
(`hermes tools enable automixer:set_track_gain`, server:tool notation). P2 must
auto-approve **only** the `automixer:*` MCP tools (which only reach our validated control
endpoint), never disable global gates.

## Deferred to P2 integration (not architectural blockers)

- Capturing a live ACP `session/update` stream and mapping chunk/tool events onto the
  `llm:*` Tauri events.
- ACP `session/cancel` → existing `cancel_agent` flag.
- Auto-approval wiring so the autonomous loop runs only `automixer:*` tools.

## Verdict

Architecture validated end-to-end. Control surface (Rust) ✓, MCP shim (Python) ✓, Hermes
discovery/enable ✓, MCP→HTTP→live-session ✓, ACP streaming transport available ✓, model
loop ✓. Proceed to P2 (audio chat through Hermes via ACP).
