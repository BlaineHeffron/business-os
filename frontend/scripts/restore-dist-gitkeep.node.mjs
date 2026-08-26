import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { restoreDistGitkeep } from "./restore-dist-gitkeep.mjs";

test("restoreDistGitkeep recreates the dist marker after a clean build", async () => {
  const root = await mkdtemp(join(tmpdir(), "bos-dist-gitkeep-"));
  const dist = join(root, "dist");

  try {
    const marker = await restoreDistGitkeep(dist);
    assert.equal(marker, join(dist, ".gitkeep"));
    assert.equal(await readFile(marker, "utf8"), "");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("restoreDistGitkeep preserves an existing marker file", async () => {
  const root = await mkdtemp(join(tmpdir(), "bos-dist-gitkeep-"));
  const dist = join(root, "dist");
  const marker = join(dist, ".gitkeep");

  try {
    await restoreDistGitkeep(dist);
    await writeFile(marker, "keep me");

    assert.equal(await restoreDistGitkeep(dist), marker);
    assert.equal(await readFile(marker, "utf8"), "keep me");
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
