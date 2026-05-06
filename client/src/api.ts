import type { AssistantRequest, AssistantResponse, MixAction, MixProject, MixSession, SkillCatalog } from "../../shared/types";

async function json<T>(input: RequestInfo, init?: RequestInit): Promise<T> {
  const response = await fetch(input, init);
  if (!response.ok) {
    const body = await response.json().catch(() => undefined);
    throw new Error(body?.error ?? response.statusText);
  }
  return response.json() as Promise<T>;
}

export const api = {
  health: () => json<{ ok: boolean }>("/api/health"),
  skills: () => json<SkillCatalog>("/api/skills"),
  sessions: () => json<MixSession[]>("/api/sessions"),
  createSession: (name: string) =>
    json<MixProject>("/api/sessions", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name })
    }),
  getSession: (id: string) => json<MixProject>(`/api/sessions/${id}`),
  importFiles: (sessionId: string, files: File[]) => {
    const form = new FormData();
    for (const file of files) form.append("files", file);
    return json<MixProject>(`/api/sessions/${sessionId}/import`, { method: "POST", body: form });
  },
  applyActions: (sessionId: string, actions: MixAction[], explanation?: string) =>
    json<MixProject>(`/api/sessions/${sessionId}/actions`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ actions, explanation })
    }),
  undo: (sessionId: string) => json<MixProject>(`/api/sessions/${sessionId}/undo`, { method: "POST" }),
  redo: (sessionId: string) => json<MixProject>(`/api/sessions/${sessionId}/redo`, { method: "POST" }),
  assistant: (request: AssistantRequest) =>
    json<AssistantResponse>("/api/assistant", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(request)
    })
};
