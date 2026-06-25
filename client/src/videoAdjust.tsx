// Photos-style color adjustments for live video preview, using ONLY built-in
// CSS filter functions (plus overlays for vignette/grain).
//
// Why pure CSS: Tauri serves clips cross-origin (asset://…), so WebGL/2D-canvas
// pixel access is blocked, and referencing an inline SVG <filter> via
// `filter: url(#id)` is unreliable in WKWebView — and because CSS `filter` is
// all-or-nothing, one unsupported token voids the whole property (every slider
// stops working). Built-in CSS functions always apply. The trade-off: gamma,
// highlights, shadows and sharpen have no exact CSS equivalent, so the PREVIEW
// approximates them with brightness/contrast — the EXPORT bakes the real ffmpeg
// filters (layout_processing_suffix in src-tauri/src/commands.rs).
import type { CSSProperties } from "react";
import type { VideoLayout } from "../../shared/types";

export type Grade = VideoLayout;

const clampNum = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));

/** Build the CSS `filter` value for the <video> from a grade. */
export function cssAdjustFilter(g: Grade): string {
  const exposure = clampNum(g.exposure ?? 0, -1, 1);
  const gamma = clampNum(g.gamma ?? 1, 0.5, 1.8);
  const hl = clampNum(g.highlights ?? 0, -1, 1);
  const sh = clampNum(g.shadows ?? 0, -1, 1);
  const sharpen = clampNum(g.sharpen ?? 0, 0, 2);

  // Fold exposure + gamma + highlights/shadows approximations into brightness,
  // and sharpen/tonal into contrast (CSS has no gamma or tone curve).
  let brightness = (g.brightness ?? 1) * Math.pow(2, exposure);
  brightness *= 1 + (gamma - 1) * 0.4 + hl * 0.12 + sh * 0.1;
  const contrast = (g.contrast ?? 1) * (1 + hl * 0.06 - sh * 0.06 + sharpen * 0.05);

  const parts = [
    `brightness(${brightness.toFixed(4)})`,
    `contrast(${contrast.toFixed(4)})`,
    `saturate(${(g.saturation ?? 1).toFixed(4)})`,
  ];
  // Temperature/tint are a color CAST → handled by whiteBalanceStyle overlay
  // (CSS filter funcs can't add a cast; hue-rotate just spins hues, ~invisible).
  if ((g.blur ?? 0) > 0) parts.push(`blur(${g.blur}px)`);
  return parts.join(" ");
}

/** Colored overlay implementing white balance (temperature warm/cool + tint
 *  green/magenta) as a real color cast, like Photos' White Balance. */
export function whiteBalanceStyle(g: Grade): CSSProperties | null {
  const temp = clampNum(g.temperature ?? 0, -1, 1);
  const tint = clampNum(g.tint ?? 0, -1, 1);
  if (temp === 0 && tint === 0) return null;
  // temp: warm(+) = orange (R↑ B↓), cool(−) = blue (R↓ B↑).
  // tint: magenta(+) = R↑ B↑ G↓, green(−) = G↑ R↓ B↓.
  const cl = (x: number) => Math.round(Math.max(0, Math.min(255, x)));
  const r = cl(128 + temp * 80 + tint * 40);
  const gr = cl(128 - tint * 55);
  const b = cl(128 - temp * 80 + tint * 40);
  const strength = Math.min(1, Math.abs(temp) + Math.abs(tint));
  return {
    position: "absolute",
    inset: 0,
    pointerEvents: "none",
    backgroundColor: `rgb(${r}, ${gr}, ${b})`,
    mixBlendMode: "soft-light",
    opacity: strength * 0.75,
  };
}

/** Radial-darkening overlay for vignette (CSS filters can't do this). */
export function vignetteStyle(g: Grade): CSSProperties {
  const v = clampNum(g.vignette ?? 0, 0, 1);
  return {
    position: "absolute",
    inset: 0,
    pointerEvents: "none",
    background: `radial-gradient(ellipse at center, transparent ${(100 - v * 70).toFixed(0)}%, rgba(0,0,0,${(v * 0.85).toFixed(3)}) 100%)`,
  };
}

const GRAIN_SVG = encodeURIComponent(
  '<svg xmlns="http://www.w3.org/2000/svg" width="140" height="140"><filter id="n"><feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" stitchTiles="stitch"/><feColorMatrix type="matrix" values="0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0.6 0"/></filter><rect width="100%" height="100%" filter="url(#n)"/></svg>',
);

/** Noise overlay for grain. */
export function grainStyle(g: Grade): CSSProperties {
  const amt = clampNum(g.grain ?? 0, 0, 1);
  return {
    position: "absolute",
    inset: 0,
    pointerEvents: "none",
    backgroundImage: `url("data:image/svg+xml,${GRAIN_SVG}")`,
    opacity: amt * 0.6,
    mixBlendMode: "overlay",
  };
}
