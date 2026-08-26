import { mkdir, open } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const scriptDir = dirname(scriptPath);
const frontendDir = dirname(scriptDir);
const defaultDistDir = join(frontendDir, "dist");

export async function restoreDistGitkeep(distDir = defaultDistDir) {
  await mkdir(distDir, { recursive: true });
  const marker = join(distDir, ".gitkeep");
  const handle = await open(marker, "a");
  try {
    return marker;
  } finally {
    await handle.close();
  }
}

if (process.argv[1] && scriptPath === resolve(process.argv[1])) {
  await restoreDistGitkeep();
}
