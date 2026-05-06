import fs from "node:fs/promises";
import path from "node:path";
import cors from "cors";
import express from "express";
import multer from "multer";
import { v4 as uuid } from "uuid";
import type { AssistantRequest, MixAction } from "../../shared/types";
import { applyActions, redo, undo, validateActions } from "./actions";
import { handleAssistant } from "./assistant";
import { buildCapabilitySnapshot, selectSkills, skillCatalog } from "./capabilities";
import { loadConfig } from "./config";
import { SessionStore } from "./store";

const config = loadConfig();
const store = new SessionStore(config.dataDir);
await store.init();
await fs.mkdir(store.uploadsDir, { recursive: true });

const upload = multer({
  storage: multer.diskStorage({
    destination: (_req, _file, cb) => cb(null, store.uploadsDir),
    filename: (_req, file, cb) => cb(null, `${uuid()}${path.extname(file.originalname)}`)
  })
});

const app = express();
app.use(cors());
app.use(express.json({ limit: "10mb" }));

app.get("/", (_req, res) => {
  res.type("html").send(`
    <!doctype html>
    <html lang="en">
      <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <title>AutoMixer API</title>
        <style>
          body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: #0d1014; color: #e8edf2; font-family: system-ui, sans-serif; }
          main { max-width: 520px; padding: 24px; line-height: 1.5; }
          a { color: #7fb4ff; }
          code { background: #171d24; padding: 2px 6px; border-radius: 4px; }
        </style>
      </head>
      <body>
        <main>
          <h1>AutoMixer API</h1>
          <p>The API is running. Open the app at <a href="http://127.0.0.1:5173/">http://127.0.0.1:5173/</a>.</p>
          <p>Health check: <a href="/api/health"><code>/api/health</code></a></p>
        </main>
      </body>
    </html>
  `);
});

app.get("/api/health", (_req, res) => {
  res.json({ ok: true, service: "automixer", mode: "node-poc" });
});

app.get("/api/skills", (_req, res) => {
  res.json(skillCatalog);
});

app.get("/api/sessions", async (_req, res, next) => {
  try {
    res.json(await store.listSessions());
  } catch (error) {
    next(error);
  }
});

app.post("/api/sessions", async (req, res, next) => {
  try {
    const project = await store.createSession(req.body?.name ?? "Untitled mix");
    res.json(project);
  } catch (error) {
    next(error);
  }
});

app.get("/api/sessions/:id", async (req, res, next) => {
  try {
    res.json(await store.getProject(req.params.id));
  } catch (error) {
    next(error);
  }
});

app.get("/api/sessions/:id/capabilities", async (req, res, next) => {
  try {
    const project = await store.getProject(req.params.id);
    const skills = typeof req.query.skills === "string" ? req.query.skills.split(",").filter(Boolean) : selectSkills("");
    res.json(buildCapabilitySnapshot(project.session, skills));
  } catch (error) {
    next(error);
  }
});

app.post("/api/sessions/:id/import", upload.array("files"), async (req, res, next) => {
  try {
    const sessionId = String(req.params.id);
    let project = await store.getProject(sessionId);
    const files = (req.files ?? []) as Express.Multer.File[];
    for (const file of files) {
      project = await store.addSourceFile(sessionId, file);
    }
    res.json(project);
  } catch (error) {
    next(error);
  }
});

app.get("/api/files/:storedName", (req, res) => {
  res.sendFile(path.join(store.uploadsDir, req.params.storedName));
});

app.post("/api/sessions/:id/actions", async (req, res, next) => {
  try {
    const project = await store.getProject(req.params.id);
    const actions = req.body?.actions as MixAction[];
    validateActions(project.session, actions);
    const result = applyActions(project, actions, "user", req.body?.explanation);
    if (result.entry) await store.pushHistory(project, result.entry);
    else await store.save(project);
    res.json(project);
  } catch (error) {
    next(error);
  }
});

app.post("/api/sessions/:id/undo", async (req, res, next) => {
  try {
    const project = await store.getProject(req.params.id);
    undo(project);
    await store.save(project);
    res.json(project);
  } catch (error) {
    next(error);
  }
});

app.post("/api/sessions/:id/redo", async (req, res, next) => {
  try {
    const project = await store.getProject(req.params.id);
    redo(project);
    await store.save(project);
    res.json(project);
  } catch (error) {
    next(error);
  }
});

app.post("/api/assistant", async (req, res, next) => {
  try {
    res.json(await handleAssistant(store, config, req.body as AssistantRequest));
  } catch (error) {
    next(error);
  }
});

app.use((error: unknown, _req: express.Request, res: express.Response, _next: express.NextFunction) => {
  const message = error instanceof Error ? error.message : "Unexpected server error";
  res.status(400).json({ error: message });
});

app.listen(config.port, "127.0.0.1", () => {
  console.log(`AutoMixer API listening on http://127.0.0.1:${config.port}`);
});
