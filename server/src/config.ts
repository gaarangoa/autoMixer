import path from "node:path";

export type Config = {
  port: number;
  dataDir: string;
  ollamaBaseUrl: string;
  ollamaModel: string;
};

export function loadConfig(): Config {
  const dataDir = process.env.DATA_DIR ?? "./data";
  return {
    port: Number(process.env.PORT ?? 5178),
    dataDir: path.resolve(dataDir),
    ollamaBaseUrl: process.env.OLLAMA_BASE_URL ?? "http://localhost:11434",
    ollamaModel: process.env.OLLAMA_MODEL ?? "gpt-oss:20b"
  };
}
