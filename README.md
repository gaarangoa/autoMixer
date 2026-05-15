# AutoMixer

AutoMixer is a proprietary desktop mixing workstation for assisted and autonomous
audio mixing. It combines a Tauri 2 desktop shell, a React timeline/mixer UI, a
Rust audio engine, local LLM control through Ollama, optional Gemini A/B mix
critique, and a Python audio-analysis sidecar for song-structure detection.

The app is designed around real multitrack sessions: import stems, record takes,
edit track regions, audition pre/post AI processing, ask the agent for mix
changes, or run the staged autonomous mix workflow.

## License

This is proprietary commercial software. See [LICENSE](LICENSE). No commercial
use, redistribution, derivative work, or commercial derivative use is permitted
unless separately authorized in writing by the copyright holder.

## Requirements

- macOS, Windows, or Linux with the platform dependencies required by Tauri 2.
- Node.js 20 or newer.
- Rust stable toolchain with Cargo.
- `uv` for the Python audio-analysis sidecar.
- Python 3.11 for the sidecar environment.
- Ollama if you want local agent/autonomous mixing.

Optional:

- A Gemini API key for cloud A/B mix critique. The app can also use the local
  QC path without Gemini.

## Install Dependencies

From the repository root:

```sh
npm install
```

Install the audio-analysis sidecar dependencies once:

```sh
cd audio-service
uv sync --python 3.11
cd ..
```

For local LLM use, install Ollama and pull the default model:

```sh
ollama pull gpt-oss:20b
```

## Configuration

The app has sensible local defaults:

- Ollama URL: `http://localhost:11434`
- Ollama model: `gpt-oss:20b`
- Audio sidecar port: `7321`

You can copy `.env.example` to `.env.local` for local overrides:

```sh
cp .env.example .env.local
```

Useful environment variables:

```sh
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=gpt-oss:20b
GEMINI_API_KEY=...
AUTOMIXER_AUDIO_PORT=7321
AUTOMIXER_UV=/path/to/uv
```

The Gemini key can also be entered in the app settings.

## Run for Development

Start the desktop app:

```sh
npm run dev
```

This starts Vite, runs the Tauri desktop shell, builds the Rust backend, and
spawns the Python audio-analysis sidecar automatically. Run the sidecar manually
only when debugging it directly:

```sh
cd audio-service
uv run uvicorn main:app --host 127.0.0.1 --port 7321
```

## Build

Build the frontend only:

```sh
npm run build
```

Build the installable desktop app:

```sh
npm run tauri build
```

Tauri writes release artifacts under:

```sh
src-tauri/target/release/bundle/
```

On macOS this includes `.dmg` and `.app` artifacts when bundling succeeds. The
exact output depends on the host platform and Tauri target configuration.

## Install

After `npm run tauri build`, install the artifact for your platform:

- macOS: open the generated `.dmg` in `src-tauri/target/release/bundle/dmg/`
  and drag `AutoMixer.app` into Applications, or run the `.app` from the bundle
  output.
- Windows: run the generated installer from the bundle output.
- Linux: install or run the generated package/AppImage from the bundle output.

For development installs, running `npm run dev` is usually enough.

## Smoke Checks

Before packaging a release, run:

```sh
npm run build
cargo check --manifest-path src-tauri/Cargo.toml
```

## Project Layout

```text
src-tauri/       Rust backend, audio engine, commands, Tauri shell
client/          React and TypeScript frontend
shared/          Shared TypeScript model types
audio-service/   Python FastAPI sidecar for structure analysis
LICENSE          Proprietary commercial license
```
