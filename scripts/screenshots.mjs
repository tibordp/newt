#!/usr/bin/env node
// Capture the scenes in docs/scenes/library as PNGs in macOS window chrome.
// See docs/scenes/README.md.
//
//   npm run screenshots -- [scene...] [--out DIR] [--scheme light|dark]
//
// Needs Playwright's WebKit once: `npx playwright install webkit`.

import { spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { webkit } from "playwright";
import { createServer } from "vite";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const { values: opts, positionals: only } = parseArgs({
  allowPositionals: true,
  options: {
    out: { type: "string", default: join(root, "docs", "screenshots") },
    scheme: { type: "string" },
  },
});

// Default preferences come from Rust so shortcuts, columns and every other
// default match the app. Keybinding defaults are per-platform: the macOS
// house style wants this run on a Mac.
mkdirSync(join(root, "target", "scenes"), { recursive: true });
const exported = spawnSync(
  "cargo",
  [
    "run",
    "-p",
    "newt",
    "--features",
    "specta-bindings",
    "--quiet",
    "--",
    "--export-scene-defaults",
    join(root, "target", "scenes", "defaults.json"),
  ],
  { cwd: root, stdio: "inherit" },
);
if (exported.status !== 0) process.exit(exported.status ?? 1);

const server = await createServer({
  root,
  logLevel: "warn",
  server: { port: 1430, strictPort: false },
});
await server.listen();
const base = server.resolvedUrls.local[0];
const browser = await webkit.launch();
mkdirSync(opts.out, { recursive: true });

let failed = false;
try {
  const index = await browser.newPage();
  await index.goto(new URL("docs/scenes/frame.html", base).href);
  const scenes = await index.evaluate(() => window.__scenes);
  await index.close();

  const unknown = only.filter((n) => !(n in scenes));
  if (unknown.length) throw new Error(`unknown scene(s): ${unknown.join(", ")}`);

  for (const [name, { schemes }] of Object.entries(scenes)) {
    if (only.length && !only.includes(name)) continue;
    for (const scheme of schemes) {
      if (opts.scheme && scheme !== opts.scheme) continue;
      const out = join(opts.out, `${name}${scheme === "dark" ? "-dark" : ""}.png`);
      try {
        await capture(name, scheme, out);
        console.log(`✓ ${out}`);
      } catch (e) {
        failed = true;
        console.error(`✗ ${name} (${scheme}): ${e.message}`);
      }
    }
  }
} finally {
  await browser.close();
  await server.close();
}
process.exit(failed ? 1 : 0);

async function capture(name, scheme, out) {
  const context = await browser.newContext({
    viewport: { width: 1600, height: 1200 },
    deviceScaleFactor: 2,
    colorScheme: scheme,
    locale: "en-US",
    timezoneId: "Europe/Ljubljana",
  });
  try {
    const page = await context.newPage();
    page.on("console", (msg) => {
      if (msg.type() === "error") console.error(`  ${name}: ${msg.text()}`);
    });
    page.on("pageerror", (e) => console.error(`  ${name}: ${e.message}`));
    await page.clock.setFixedTime(new Date("2026-07-28T17:35:00+02:00"));
    await page.goto(
      new URL(`docs/scenes/frame.html?scene=${encodeURIComponent(name)}`, base)
        .href,
    );
    const app = await (await page.waitForSelector("iframe")).contentFrame();
    await app.waitForFunction(() => window.__scene);
    await settle(app);

    const { steps } = await app.evaluate(() => window.__scene);
    for (const step of steps) {
      if ("press" in step) await page.keyboard.press(step.press);
      else if ("type" in step) await page.keyboard.type(step.type);
      else if ("click" in step) await app.click(step.click);
      else if ("waitFor" in step) await app.waitForSelector(step.waitFor);
    }
    if (steps.length) await settle(app);

    const { errors, unmocked } = await app.evaluate(() => window.__scene);
    if (unmocked.length) console.warn(`  ${name}: unmocked ${unmocked.join(", ")}`);
    if (errors.length) throw new Error(errors.join("; "));

    await (await page.$(".backdrop")).screenshot({ path: out, omitBackground: true });
  } finally {
    await context.close();
  }
}

async function settle(app) {
  await app.evaluate(async () => {
    const scene = window.__scene;
    await Promise.race([
      scene.settled(),
      new Promise((_, reject) =>
        setTimeout(
          () => reject(new Error(scene.errors.join("; ") || "never settled")),
          20_000,
        ),
      ),
    ]);
  });
}
