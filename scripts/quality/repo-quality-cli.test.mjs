import { spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const fixtures = join(root, 'scripts', 'quality', 'fixtures');
const cli = join(root, 'scripts', 'repo-quality-check.mjs');

async function initGit(repo) {
  spawnSync('git', ['init', '--quiet'], { cwd: repo });
  spawnSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'add', '.'], { cwd: repo });
  spawnSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '--quiet', '-m', 'fixture'], { cwd: repo });
}

function dualConfig() {
  return {
    repo: 'BusinessOS frontend',
    outputDir: '.quality-frontend',
    coverage: { format: 'istanbul-json', output: 'inputs/frontend-coverage.json', command: 'unused' },
    complexity: { format: 'eslint-json', output: 'inputs/frontend-eslint.json', command: 'unused' },
    thresholds: { crapFail: 30 },
    rust: {
      repo: 'BusinessOS rust',
      outputDir: '.quality-rust',
      coverage: { format: 'llvm-cov-json', output: 'inputs/llvm-cov.json', command: 'unused' },
      complexity: { format: 'rust-code-analysis-json', output: '{outputDir}/rust-metrics', command: 'unused' },
    },
  };
}

async function makeDualRepo() {
  const repo = await mkdtemp(join(tmpdir(), 'bos-quality-cli-'));
  await mkdir(join(repo, 'inputs'), { recursive: true });
  const frontendCoverage = (await readFile(join(fixtures, 'frontend-coverage.json'), 'utf8'))
    .replaceAll('/repo', repo);
  const frontendEslint = (await readFile(join(fixtures, 'frontend-eslint.json'), 'utf8'))
    .replaceAll('/repo', repo);
  const llvm = (await readFile(join(fixtures, 'crates-llvm-cov.json'), 'utf8'))
    .replaceAll('/repo', repo);
  await writeFile(join(repo, 'inputs', 'frontend-coverage.json'), frontendCoverage);
  await writeFile(join(repo, 'inputs', 'frontend-eslint.json'), frontendEslint);
  await writeFile(join(repo, 'inputs', 'llvm-cov.json'), llvm);
  await mkdir(join(repo, '.quality-rust', 'rust-metrics', 'crates', 'bos-app', 'src'), { recursive: true });
  await mkdir(join(repo, '.quality-rust', 'rust-metrics', 'apps', 'bos-server', 'src'), { recursive: true });
  const libMetrics = (await readFile(join(fixtures, 'crates-rust-code-analysis-lib.json'), 'utf8'))
    .replaceAll('/repo', repo);
  const mainMetrics = (await readFile(join(fixtures, 'crates-rust-code-analysis-main.json'), 'utf8'))
    .replaceAll('/repo', repo);
  await writeFile(join(repo, '.quality-rust', 'rust-metrics', 'crates', 'bos-app', 'src', 'lib.rs.json'), libMetrics);
  await writeFile(join(repo, '.quality-rust', 'rust-metrics', 'apps', 'bos-server', 'src', 'main.rs.json'), mainMetrics);
  await writeFile(join(repo, '.quality-gates.json'), `${JSON.stringify(dualConfig(), null, 2)}\n`);
  await initGit(repo);
  return repo;
}

function runCli(repo, extraArgs) {
  return spawnSync(process.execPath, [cli, '--repo', repo, '--no-run', '--json', ...extraArgs], {
    encoding: 'utf8',
  });
}

describe('repo quality CLI sections', () => {
  it('keeps the default frontend istanbul/eslint adapters when --section is omitted', async () => {
    const repo = await makeDualRepo();
    const result = runCli(repo, []);
    assert.equal(result.status, 0, result.stderr);
    const report = JSON.parse(result.stdout);
    assert.equal(report.repo, 'BusinessOS frontend');
    assert.equal(report.totals.functions, 1);
    assert.equal(report.functions[0].name, 'render');
    assert.equal(report.functions[0].file, 'frontend/src/app.ts');
  });

  it('does not inherit frontend istanbul adapters when --section rust is set', async () => {
    const repo = await makeDualRepo();
    const result = runCli(repo, ['--section', 'rust']);
    assert.equal(result.status, 0, result.stderr);
    const report = JSON.parse(result.stdout);
    assert.equal(report.repo, 'BusinessOS rust');
    assert.ok(!report.functions.some((fn) => String(fn.file).includes('frontend')));
  });

  it('selects the rust coverage/complexity section with --section rust', async () => {
    const repo = await makeDualRepo();
    const result = runCli(repo, ['--section', 'rust']);
    assert.equal(result.status, 0, result.stderr);
    const report = JSON.parse(result.stdout);
    assert.equal(report.repo, 'BusinessOS rust');
    assert.deepEqual(report.functions.map((fn) => fn.name).sort(), ['prod_fn', 'server_fn']);
    assert.equal(report.functions.find((fn) => fn.name === 'prod_fn').cc, 4);
    assert.equal(report.functions.find((fn) => fn.name === 'server_fn').cc, 3);
  });

  it('fails closed for an unknown --section', async () => {
    const repo = await makeDualRepo();
    const result = runCli(repo, ['--section', 'python']);
    assert.equal(result.status, 2, result.stderr);
    assert.match(result.stderr, /unknown quality-gates section: python/);
  });

  it('fails closed when llvm-cov JSON is not the export document type', async () => {
    const repo = await makeDualRepo();
    await writeFile(join(repo, 'inputs', 'llvm-cov.json'), `${JSON.stringify({ data: [] })}\n`);
    const result = runCli(repo, ['--section', 'rust']);
    assert.equal(result.status, 2, result.stderr);
    assert.match(result.stderr, /llvm.coverage.json.export/);
  });

  it('keeps --json stdout parseable when a rust adapter writes to stdout', async () => {
    const repo = await makeDualRepo();
    const config = dualConfig();
    config.rust.coverage.command = "printf 'ADAPTER_STDOUT_MARKER\\n' && mkdir -p {outputDir} && cp inputs/llvm-cov.json {outputDir}/llvm-cov.json";
    config.rust.coverage.output = '{outputDir}/llvm-cov.json';
    config.rust.complexity.command = 'mkdir -p {outputDir}/rust-metrics/crates/bos-app/src {outputDir}/rust-metrics/apps/bos-server/src && cp inputs/lib.rs.json {outputDir}/rust-metrics/crates/bos-app/src/lib.rs.json && cp inputs/main.rs.json {outputDir}/rust-metrics/apps/bos-server/src/main.rs.json';
    await writeFile(join(repo, 'inputs', 'lib.rs.json'), await readFile(join(repo, '.quality-rust', 'rust-metrics', 'crates', 'bos-app', 'src', 'lib.rs.json')));
    await writeFile(join(repo, 'inputs', 'main.rs.json'), await readFile(join(repo, '.quality-rust', 'rust-metrics', 'apps', 'bos-server', 'src', 'main.rs.json')));
    await writeFile(join(repo, '.quality-gates.json'), `${JSON.stringify(config, null, 2)}\n`);

    const result = spawnSync(process.execPath, [cli, '--repo', repo, '--json', '--section', 'rust'], {
      encoding: 'utf8',
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stderr, /ADAPTER_STDOUT_MARKER/);
    assert.doesNotMatch(result.stdout, /ADAPTER_STDOUT_MARKER/);
    const report = JSON.parse(result.stdout);
    assert.equal(report.repo, 'BusinessOS rust');
    assert.deepEqual(report.functions.map((fn) => fn.name).sort(), ['prod_fn', 'server_fn']);
  });
});
