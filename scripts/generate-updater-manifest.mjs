import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";
import { pathToFileURL } from "node:url";

const updaterAssets = {
  "darwin-aarch64": "worktree-gc-updater-darwin-aarch64.tar.gz",
  "darwin-x86_64": "worktree-gc-updater-darwin-x86_64.tar.gz",
  "windows-x86_64": "worktree-gc-updater-windows-x86_64.exe",
  "linux-x86_64": "worktree-gc-updater-linux-x86_64.AppImage",
};

export function generateUpdaterManifest({ assetsDir, tag, repository, pubDate }) {
  const version = tag.replace(/^v/, "");
  if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`invalid release tag: ${tag}`);
  }
  if (!/^[^/]+\/[^/]+$/.test(repository)) {
    throw new Error(`invalid GitHub repository: ${repository}`);
  }
  if (Number.isNaN(Date.parse(pubDate))) {
    throw new Error(`invalid publication date: ${pubDate}`);
  }

  const platforms = Object.fromEntries(Object.entries(updaterAssets).map(([platform, name]) => {
    const artifact = join(assetsDir, name);
    const signatureFile = `${artifact}.sig`;
    if (!existsSync(artifact) || !existsSync(signatureFile)) {
      throw new Error(`missing updater artifact or signature for ${platform}`);
    }
    const signature = readFileSync(signatureFile, "utf8").trim();
    if (!signature) throw new Error(`empty updater signature for ${platform}`);
    const url = `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(basename(artifact))}`;
    return [platform, { signature, url }];
  }));

  return {
    version,
    notes: `worktree-gc ${tag} is available. See the GitHub Release for details.`,
    pub_date: new Date(pubDate).toISOString(),
    platforms,
  };
}

function valueAfter(args, flag) {
  const index = args.indexOf(flag);
  if (index < 0 || !args[index + 1]) throw new Error(`missing ${flag}`);
  return args[index + 1];
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const assetsDir = valueAfter(process.argv, "--assets");
  const output = valueAfter(process.argv, "--output");
  const manifest = generateUpdaterManifest({
    assetsDir,
    tag: valueAfter(process.argv, "--tag"),
    repository: valueAfter(process.argv, "--repository"),
    pubDate: valueAfter(process.argv, "--pub-date"),
  });
  writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`);
}
