# AutoMixer Model Benchmark

Evaluates how well an LLM maps natural-language mix requests to AutoMixer's action schema.

## Files

- `sessions.json` — reusable session templates (tracks + audio analysis values).
- `cases.json` — 100 prompts with expected skills and expected actions.
- `run.mjs` — Node runner that fires each case at an Ollama model and scores it.

## Case schema

```json
{
  "id": "001",
  "complexity": "simple|moderate|complex|advanced",
  "session": "rock5",
  "selectedTrackIds": ["tk0"],
  "selectedRegionIds": [],
  "userText": "make the vocal louder",
  "expected": {
    "skills": ["balance"],
    "actions": [
      { "tool": "adjust_track_gain", "trackRole": "lead_vocal", "deltaDb": 1.5 }
    ],
    "tolerances": { "deltaDb": 1.0, "gainDb": 1.5, "frequencyHz": 0.4, "ratio": 0.8, "ms": 30, "q": 0.3, "levelDb": 3 }
  }
}
```

`expected.actions` use `trackRole` (or `trackName`) instead of UUIDs so cases are session-agnostic. The runner resolves them to the actual track id before comparing.

`tolerances` define how much numeric drift is acceptable per field type. `frequencyHz: 0.4` means within 40% of the target (so 3 kHz target accepts 1.8–4.2 kHz). Defaults apply when a field is omitted.

## Complexity tiers

| Tier | Count | Definition |
|---|---|---|
| simple | 40 | Single direct action on a single track. |
| moderate | 40 | Compound intent → 2–3 actions on one track. |
| complex | 15 | Multi-track moves or section-scoped automation. |
| advanced | 5 | Analysis-driven reasoning required (e.g. fix a stated imbalance). |

## Scoring

Per case, the runner produces a record with:

- `skillsHit` — fraction of expected skills present in the model's `selectedSkills`.
- `toolsHit` — fraction of expected tools matched (allowing equivalent tools listed in `actions[i].acceptableTools`).
- `targetHit` — fraction of expected actions whose track resolves correctly.
- `valuesHit` — fraction of numeric params within tolerance.
- `passed` — true when skillsHit ≥ 0.5, toolsHit ≥ 0.7, targetHit = 1, valuesHit ≥ 0.6.

The runner prints a summary table per complexity tier and writes `results-<model>.json`.

## Running

```bash
cd benchmark
OLLAMA_URL=http://localhost:11434 MODEL=gemma3:4b node run.mjs
```

Optional flags via env:
- `OLLAMA_URL` — defaults to `http://localhost:11434`.
- `MODEL` — Ollama model tag, required.
- `LIMIT` — only run the first N cases (handy for smoke-tests).
- `TIER` — comma-separated complexity tiers to run (e.g. `simple,moderate`).

## Adding cases

Append to `cases.json`. Use `trackRole` (preferred) or `trackName` so the case stays portable across sessions. If multiple tools are reasonable for the same intent, list them under `acceptableTools` on that action item.
