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

The current turnkey release supports **Apple Silicon macOS**. A clean Mac only
needs the generated AutoMixer DMG: the app bundle carries pinned copies of `uv`,
FFmpeg, FFprobe, and llama.cpp. The Setup Assistant installs the isolated Python
environments under `~/.automixer` and verifies every component. Homebrew, a
system Python, Ollama, vLLM, and LM Studio are not required.

Building AutoMixer from source requires Node.js 20 or newer and the stable Rust
toolchain with Cargo. See [Clean Mac installation](docs/CLEAN_INSTALL_MACOS.md)
for the end-user path.

Optional:

- A Gemini API key for cloud A/B mix critique. The app can also use the local
  QC path without Gemini.

## Development Dependencies

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

At startup, AutoMixer checks these three executables and reports their resolved
paths and versions. Packaged macOS builds prefer the pinned binaries inside the
app. Development builds can also use explicit `AUTOMIXER_*` overrides, the
standard Homebrew locations, or `PATH`.

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
configuration serves Qwen3.6 and its vision projector at
`http://127.0.0.1:2261`. New installs keep the managed files under
`~/.automixer/models/`; setup can adopt verified files from the historical
`~/vLLM/models/` directory without downloading them again. The legacy directory
name does not mean vLLM is used.

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

The build stages pinned Apple-Silicon runtimes, verifies their SHA-256 digests,
and bundles them into the app before Tauri creates the release artifacts.

Tauri writes release artifacts under:

```sh
src-tauri/target/release/bundle/
```

On macOS this includes `.dmg` and `.app` artifacts when bundling succeeds. The
exact output depends on the host platform and Tauri target configuration.

## Release Automation

GitHub Actions can build release bundles when you push a version tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The release workflow creates a draft GitHub Release and uploads the platform
installers generated by Tauri. You can also start the workflow manually from the
GitHub Actions tab by entering an existing tag.

This project intentionally does not currently perform Developer ID signing or
Apple notarization. Local macOS builds receive Tauri's ad-hoc signature only.

## Install

After `npm run tauri build`, open the DMG under
`src-tauri/target/release/bundle/dmg/` and drag AutoMixer into Applications.
Because this build is not notarized, the first launch requires macOS's explicit
**Open** / **Open Anyway** confirmation. The first-run Setup Assistant then:

1. verifies the bundled uv, FFmpeg, and FFprobe tools;
2. installs pinned Hermes ACP and isolated Python environments;
3. connects a remote OpenAI-compatible model endpoint, or optionally downloads
   and configures the managed local llama.cpp model;
4. installs the local model LaunchAgent when local mode is selected; and
5. checks the media tools, agent, audio service, and model endpoint before
   declaring setup complete.

The local model download is checksum-verified and resumable. SAM-Audio remains
an optional separately configured remote service.

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
