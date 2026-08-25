import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { generateUpdaterManifest } from "./generate-updater-manifest.mjs";

const names = [
  "worktree-gc-updater-darwin-aarch64.tar.gz",
  "worktree-gc-updater-darwin-x86_64.tar.gz",
  "worktree-gc-updater-windows-x86_64.exe",
  "worktree-gc-updater-linux-x86_64.AppImage",
];

function fixture() {
  const dir = mkdtempSync(join(tmpdir(), "worktree-gc-updater-"));
  for (const name of names) {
    writeFileSync(join(dir, name), "artifact");
    writeFileSync(join(dir, `${name}.sig`), `signature-${name}\n`);
  }
  return dir;
}

test("generates a complete static Tauri updater manifest", () => {
  const manifest = generateUpdaterManifest({
    assetsDir: fixture(),
    tag: "v0.1.8",
    repository: "LonelyFellas/worktree-gc",
    pubDate: "2026-08-25T08:00:00Z",
  });

  assert.equal(manifest.version, "0.1.8");
  assert.equal(manifest.pub_date, "2026-08-25T08:00:00.000Z");
  assert.deepEqual(Object.keys(manifest.platforms).sort(), [
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-x86_64",
    "windows-x86_64",
  ]);
  assert.match(manifest.platforms["darwin-aarch64"].url, /releases\/download\/v0\.1\.8\//);
  assert.match(manifest.platforms["darwin-aarch64"].signature, /^signature-/);
});

test("refuses to publish a partial platform set", () => {
  const dir = fixture();
  writeFileSync(join(dir, `${names[0]}.sig`), "");

  assert.throws(
    () => generateUpdaterManifest({
      assetsDir: dir,
      tag: "v0.1.8",
      repository: "LonelyFellas/worktree-gc",
      pubDate: "2026-08-25T08:00:00Z",
    }),
    /empty updater signature/,
  );
});
