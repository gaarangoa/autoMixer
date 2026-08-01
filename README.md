# AutoMixer

AutoMixer is a proprietary desktop mixing workstation for assisted and autonomous
audio mixing. It combines a Tauri 2 desktop shell, a React timeline/mixer UI, a
Rust audio engine, local LLM control through the project-managed llama.cpp
service, optional Gemini A/B mix critique, and a Python audio-analysis sidecar
for song-structure detection.

The app is designed around real multitrack sessions: import stems, record takes,
edit track regions, audition pre/post AI processing, ask the agent for mix
changes, or run the staged autonomous mix workflow.

## Album Documents

AutoMixer does not maintain or reopen a global album library. It starts empty;
use **Create Album** to choose a parent directory or **Open Album** to select an
existing album folder. Closing an album only closes the document and never
deletes its files.

An album is portable and self-contained:

```text
Album Name/
  album.json
  Song Name/
    song.json
    Audio/
    Peaks/
    Recordings/
    Video/
    Renders/
```

Each new song is a direct subdirectory of its album. Media references in
`song.json` are stored relative to the song directory, so the complete album
folder can be moved, copied, backed up, or opened on another machine. Deleting a
song from AutoMixer deletes that whole song directory; the confirmation dialog
calls this out explicitly.

## License

This is proprietary commercial software. See [LICENSE](LICENSE). No commercial
use, redistribution, derivative work, or commercial derivative use is permitted
unless separately authorized in writing by the copyright holder.

## Model Runtime Policy

AutoMixer uses **llama.cpp only** for its local language and vision model
runtime. Start it through the checked-in `model-service` scripts; the expected
endpoint is `http://127.0.0.1:2261`.

Ollama, vLLM, and LM Studio are not used by this project and should not be
started or selected for AutoMixer. Some internal fields and local-storage keys
still contain `ollama` in their names for backward compatibility; those names
refer to the OpenAI-compatible llama.cpp endpoint and do not indicate an Ollama
dependency.

## Requirements

- macOS, Windows, or Linux with the platform dependencies required by Tauri 2.
- Node.js 20 or newer.
- Rust stable toolchain with Cargo.
- `uv` for the Python audio-analysis sidecar.
- `ffmpeg` and `ffprobe` for video sampling, recording, and rendering.
- Python 3.11 for the sidecar environment.
- llama.cpp (`llama-server`) for local agent/autonomous mixing, managed through
  this repository's `model-service` scripts.

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

For local LLM use, the checked-in launcher uses the already-downloaded Qwen3.6
GGUF through llama.cpp:

```sh
npm run models:start
npm run models:status
```

The video agent uses the same llama.cpp deployment with its configured
`mmproj` vision projector.

## Configuration

The app has sensible local defaults:

- Model server URL: `http://127.0.0.1:2261`
- Model: `qwen3.6-35b-a3b`
- Audio sidecar port: `7321`

You can copy `.env.example` to `.env.local` for local overrides:

```sh
cp .env.example .env.local
```

Useful environment variables:

```sh
# Legacy setting names; both point to the llama.cpp OpenAI-compatible endpoint.
OLLAMA_BASE_URL=http://127.0.0.1:2261
OLLAMA_MODEL=qwen3.6-35b-a3b
GEMINI_API_KEY=...
AUTOMIXER_AUDIO_PORT=7321
AUTOMIXER_UV=/path/to/uv
AUTOMIXER_FFMPEG=/path/to/ffmpeg
AUTOMIXER_FFPROBE=/path/to/ffprobe
```

At startup, AutoMixer checks these three external executables and reports their
resolved paths and versions. Packaged macOS builds also search the standard
Homebrew locations (`/opt/homebrew/bin` and `/usr/local/bin`), because apps
opened from Finder do not inherit a shell's full `PATH`. The explicit
`AUTOMIXER_*` paths take precedence. The check only reports dependency problems;
it never installs or repairs software.

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

## Managed Local Model Server

The reproducible launcher for the llama.cpp deployment is tracked under
[`model-service/`](model-service/README.md). The current Apple-silicon
configuration serves the already-downloaded Qwen3.6 GGUF and vision projector
from the historically named `~/vLLM/models/` directory at
`http://127.0.0.1:2261`. The directory name does not mean vLLM is used.

```sh
npm run models:status
npm run models:start
npm run models:stop
```

To start it automatically whenever this user logs in after a Mac restart:

```sh
npm run models:install
```

Large model files and machine-specific `model-service/config.env` overrides are
kept outside Git.

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

## Release Automation

GitHub Actions builds release bundles for macOS, Windows, and Linux when you push
a version tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow creates a draft GitHub Release and uploads the platform
installers generated by Tauri. You can also start the workflow manually from the
GitHub Actions tab by entering an existing tag.

The generated artifacts are unsigned unless code-signing secrets are configured
separately for the target platform.

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
