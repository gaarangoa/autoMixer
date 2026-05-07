#!/usr/bin/env node
import { readFile, readdir } from "node:fs/promises";

const ROOT = new URL("./", import.meta.url);
const TIERS = ["simple", "moderate", "complex", "advanced"];

function pad(s, n, right = false) {
  s = String(s);
  if (s.length >= n) return s;
  return right ? " ".repeat(n - s.length) + s : s + " ".repeat(n - s.length);
}

async function loadResults() {
  const files = (await readdir(ROOT)).filter((f) => f.startsWith("results-") && f.endsWith(".json"));
  const out = [];
  for (const f of files) {
    const data = JSON.parse(await readFile(new URL(f, ROOT), "utf8"));
    out.push(data);
  }
  return out;
}

function tierStats(results, tier) {
  const subset = tier ? results.filter((r) => r.case.complexity === tier) : results;
  if (subset.length === 0) return null;
  const sum = (key) => subset.reduce((a, r) => a + (r.score?.[key] ?? 0), 0) / subset.length;
  const passRate = subset.filter((r) => r.score?.passed).length / subset.length;
  const avgMs = subset.reduce((a, r) => a + (r.ms ?? 0), 0) / subset.length;
  return {
    n: subset.length,
    pass: passRate,
    skills: sum("skillsHit"),
    tools: sum("toolsHit"),
    target: sum("targetHit"),
    values: sum("valuesHit"),
    avgMs,
  };
}

function fmtPct(x) {
  return (x * 100).toFixed(0) + "%";
}

function fmtMs(ms) {
  if (ms < 1000) return `${ms.toFixed(0)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function renderMarkdown(rows) {
  const headers = ["Model", "Cases", "Pass", "Skills", "Tools", "Target", "Values", "Avg/case"];
  const out = [];
  out.push("| " + headers.join(" | ") + " |");
  out.push("|" + headers.map(() => " --- ").join("|") + "|");
  for (const r of rows) {
    out.push("| " + [
      r.model,
      r.cases,
      fmtPct(r.pass),
      fmtPct(r.skills),
      fmtPct(r.tools),
      fmtPct(r.target),
      fmtPct(r.values),
      fmtMs(r.avgMs),
    ].join(" | ") + " |");
  }
  return out.join("\n");
}

async function main() {
  const datasets = await loadResults();
  if (datasets.length === 0) {
    console.error("No results-*.json files found. Run run.mjs first.");
    process.exit(1);
  }

  const summaryRows = [];
  for (const d of datasets) {
    const overall = tierStats(d.results, null);
    summaryRows.push({
      model: d.model,
      cases: overall.n,
      pass: overall.pass,
      skills: overall.skills,
      tools: overall.tools,
      target: overall.target,
      values: overall.values,
      avgMs: overall.avgMs,
    });
  }
  summaryRows.sort((a, b) => b.pass - a.pass || b.tools - a.tools);

  console.log("# Overall");
  console.log(renderMarkdown(summaryRows));
  console.log();

  for (const tier of TIERS) {
    console.log(`# Tier: ${tier}`);
    const rows = datasets.map((d) => {
      const s = tierStats(d.results, tier);
      if (!s) return null;
      return { model: d.model, cases: s.n, pass: s.pass, skills: s.skills, tools: s.tools, target: s.target, values: s.values, avgMs: s.avgMs };
    }).filter(Boolean);
    rows.sort((a, b) => b.pass - a.pass || b.tools - a.tools);
    console.log(renderMarkdown(rows));
    console.log();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
