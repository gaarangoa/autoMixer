import fs from "node:fs/promises";
import path from "node:path";
import { v4 as uuid } from "uuid";
import type { HistoryEntry, MixProject, MixSession, SourceFile } from "../../shared/types";
import { defaultMaster, makeTrack } from "./defaults";

export class SessionStore {
  private projects = new Map<string, MixProject>();

  constructor(private readonly dataDir: string) {}

  async init() {
    await fs.mkdir(this.sessionsDir, { recursive: true });
    await fs.mkdir(this.uploadsDir, { recursive: true });
  }

  get uploadsDir() {
    return path.join(this.dataDir, "uploads");
  }

  private get sessionsDir() {
    return path.join(this.dataDir, "sessions");
  }

  async createSession(name = "Untitled mix"): Promise<MixProject> {
    const session: MixSession = {
      id: uuid(),
      name,
      sampleRate: 48000,
      sourceFiles: [],
      tracks: [],
      regions: [],
      markers: [],
      master: defaultMaster()
    };
    const project: MixProject = { session, history: [], redoStack: [] };
    this.projects.set(session.id, project);
    await this.save(project);
    return project;
  }

  async getProject(sessionId: string): Promise<MixProject> {
    const cached = this.projects.get(sessionId);
    if (cached) return cached;
    const file = path.join(this.sessionsDir, `${sessionId}.json`);
    const raw = await fs.readFile(file, "utf8");
    const project = JSON.parse(raw) as MixProject;
    this.projects.set(sessionId, project);
    return project;
  }

  async listSessions(): Promise<MixSession[]> {
    await this.init();
    const files = await fs.readdir(this.sessionsDir);
    const sessions: MixSession[] = [];
    for (const file of files.filter((item) => item.endsWith(".json"))) {
      const raw = await fs.readFile(path.join(this.sessionsDir, file), "utf8");
      sessions.push((JSON.parse(raw) as MixProject).session);
    }
    return sessions.sort((a, b) => a.name.localeCompare(b.name));
  }

  async addSourceFile(sessionId: string, file: Express.Multer.File): Promise<MixProject> {
    const project = await this.getProject(sessionId);
    const sourceId = uuid();
    const source: SourceFile = {
      id: sourceId,
      originalName: file.originalname,
      storedName: file.filename,
      mimeType: file.mimetype,
      sizeBytes: file.size,
      analysis: {
        peakDb: -6,
        rmsDb: -18,
        lufsEstimate: -20,
        spectralCentroidHz: 1600,
        lowEnergy: 0.33,
        midEnergy: 0.34,
        highEnergy: 0.33,
        silencePercent: 0,
        dynamicRangeDb: 12
      }
    };
    project.session.sourceFiles.push(source);
    project.session.tracks.push(makeTrack(uuid(), sourceId, stripExtension(file.originalname), project.session.tracks.length));
    await this.save(project);
    return project;
  }

  async save(project: MixProject) {
    await fs.mkdir(this.sessionsDir, { recursive: true });
    await fs.writeFile(path.join(this.sessionsDir, `${project.session.id}.json`), JSON.stringify(project, null, 2));
  }

  async pushHistory(project: MixProject, entry: HistoryEntry) {
    project.history.push(entry);
    project.redoStack = [];
    await this.save(project);
  }
}

function stripExtension(name: string) {
  return name.replace(/\.[^.]+$/, "");
}
