import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const workflow = readFileSync(join(root, '.github', 'workflows', 'frontend-quality.yml'), 'utf8');

describe('frontend quality workflow', () => {
  it('pins a supported Node toolchain and invokes the quality script without global just', () => {
    assert.match(workflow, /uses: actions\/checkout@[0-9a-f]{40}\s+# v6\.1\.0/);
    assert.match(workflow, /uses: actions\/setup-node@[0-9a-f]{40}\s+# v6\.5\.0/);
    assert.match(workflow, /node-version: "22\.19\.0"/);
    assert.match(workflow, /cache-dependency-path: frontend\/package-lock\.json/);
    assert.match(workflow, /node --version/);
    assert.match(workflow, /npm --version/);
    assert.match(workflow, /just=not-used/);
    assert.match(workflow, /npm --prefix frontend ci/);
    assert.match(workflow, /node --test scripts\/quality\/frontend-workflow\.test\.mjs/);
    assert.match(workflow, /npm --prefix frontend run quality:crap/);
    assert.match(workflow, /scripts\/quality\/\*\*/);
    assert.doesNotMatch(workflow, /run:\s+just\b/);
    assert.doesNotMatch(workflow, /justfile/);
  });
});
