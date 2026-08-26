import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');

describe('operator-facing rust quality commands', () => {
  it('keeps frontend adapters at the top level and scopes rust coverage/complexity under a rust section', () => {
    const config = JSON.parse(readFileSync(join(root, '.quality-gates.json'), 'utf8'));
    assert.equal(config.coverage.format, 'istanbul-json');
    assert.equal(config.complexity.format, 'eslint-json');
    assert.equal(config.baseline, 'frontend/config/quality/crap-baseline.json');
    assert.equal(config.rust.coverage.format, 'llvm-cov-json');
    assert.equal(config.rust.complexity.format, 'rust-code-analysis-json');
    assert.equal(config.rust.baseline, 'scripts/quality/crap-baseline-rust.json');
    assert.match(config.rust.coverage.command, /\btimeout 1800\b/);
    assert.match(config.rust.coverage.command, /cargo llvm-cov/);
    assert.match(config.rust.coverage.command, /--ignore-run-fail/);
    assert.match(config.rust.complexity.command, /rust-code-analysis-cli/);
    assert.match(config.rust.complexity.command, /\bcrates\b/);
    assert.match(config.rust.complexity.command, /\bapps\b/);
  });

  it('exposes just coverage and just crap without changing just gate', () => {
    const justfile = readFileSync(join(root, 'justfile'), 'utf8');
    const config = JSON.parse(readFileSync(join(root, '.quality-gates.json'), 'utf8'));
    const llvmFlags = 'timeout 1800 cargo llvm-cov --workspace --json --ignore-run-fail';
    assert.match(config.rust.coverage.command, new RegExp(llvmFlags.replaceAll(' ', '\\s+')));
    assert.match(justfile, new RegExp(`^coverage:\\n(?:[ \\t].*\\n)*[ \\t]*${llvmFlags.replaceAll(' ', '\\s+')}`, 'm'));
    assert.match(justfile, /^crap:\n(?:[ \t].*\n)*[ \t]*node scripts\/repo-quality-check\.mjs --section rust/m);
    assert.match(justfile, /^gate: fmt lint test code-shape$/m);
    assert.doesNotMatch(justfile, /^gate:.*\bcrap\b/m);
    assert.doesNotMatch(justfile, /^gate:.*\bcoverage\b/m);
    assert.doesNotMatch(justfile, /^gate:.*\bmutants\b/m);
  });

  it('dry-runs just mutants-slice to a guarded 30-minute per-slice cargo-mutants command', () => {
    const result = spawnSync('just', ['-n', 'mutants-slice', 'client_profile'], {
      encoding: 'utf8',
      cwd: root,
    });
    assert.equal(result.status, 0, result.stderr || result.stdout);
    const output = `${result.stdout}${result.stderr}`;
    assert.match(output, /name='client_profile'/);
    assert.match(output, /mkdir -p "target\/mutants-\$name"/);
    assert.match(
      output,
      /timeout 1800 cargo mutants --file "crates\/bos-app\/src\/slices\/\$name\/\*\*" -j 4 --timeout-multiplier 3 --output "target\/mutants-\$name"/,
    );
    assert.match(output, /\^\[a-z0-9_\]\+\$/);
  });

  it('rejects invalid mutants-slice names without invoking cargo mutants', () => {
    const result = spawnSync('just', ['mutants-slice', '../etc'], {
      encoding: 'utf8',
      cwd: root,
    });
    assert.notEqual(result.status, 0);
    const output = `${result.stdout}${result.stderr}`;
    assert.match(output, /invalid slice name/);
    assert.doesNotMatch(output, /cargo mutants/);
  });
});
