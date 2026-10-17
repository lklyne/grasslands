// Writes cuelume's reference renders for crates/cues: raw little-endian f64 samples, one file per
// theme × cue × emphasis × part × walk step × take × channel, and manifest.json. Run through scripts/cues-refs.
//
//   node scripts/cues-refs.mjs <cuelume checkout, built> <out dir> <commit>
import { createHash } from "node:crypto";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [checkout, out, commit] = process.argv.slice(2);
if (!checkout || !out) throw new Error("usage: cues-refs.mjs <cuelume checkout> <out dir> <commit>");

const { THEMES, SR, render } = await import(pathToFileURL(resolve(checkout, "scripts/sound.mjs")).href);

const EMPHASES = ["subtle", "normal", "strong"];
/** Extra takes reseed the noise; one is enough everywhere but the canonical play. */
const TAKES = 3;

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

const files = [];
let bytes = 0;
for (const theme of Object.keys(THEMES)) {
  for (const cue of Object.keys(THEMES[theme])) {
    const recipe = THEMES[theme][cue];
    const parts = recipe.release ? ["both", "press", "release"] : ["both"];
    const steps = Math.max(1, recipe.walk?.length ?? 0);
    for (const emphasis of EMPHASES) {
      for (const part of parts) {
        for (let step = 0; step < steps; step++) {
          const takes = emphasis === "normal" && part === "both" && step === 0 ? TAKES : 1;
          for (let take = 0; take < takes; take++) {
            for (const channel of [0, 1]) {
              const samples = render(theme, cue, { emphasis, part, step, channel, seed: take });
              const data = Buffer.from(samples.buffer, samples.byteOffset, samples.byteLength);
              const file = `${theme}-${cue}-${emphasis}-${part}-s${step}-t${take}-c${channel}.f64`;
              writeFileSync(join(out, file), data);
              bytes += data.length;
              files.push({
                file, theme, cue, emphasis, part, step, take, channel,
                length: samples.length,
                sha256: createHash("sha256").update(data).digest("hex"),
              });
            }
          }
        }
      }
    }
  }
}

const manifest = { source: "https://github.com/danielwh2/cuelume", commit, rate: SR, format: "f64le", files };
writeFileSync(join(out, "manifest.json"), JSON.stringify(manifest, null, 1) + "\n");
console.log(`wrote ${files.length} renders, ${(bytes / 1e6).toFixed(1)} MB, to ${out}`);
