#!/usr/bin/env node

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path';

import {
  buildQualityReport,
  buildRatchetBaseline,
  compareQualityRatchet,
  filterProductionRustFunctions,
  formatQualityMarkdown,
  joinFunctionMetrics,
  parseCargoMutantsOutcomes,
  parseEslintComplexity,
  parseIstanbulCoverage,
  parseLlvmCovCoverage,
  parseRustCodeAnalysisComplexity,
  compareMutationThresholds,
} from './repo-quality.mjs';

function usageError(message) {
  const error = new Error(message);
  error.exitCode = 2;
  return error;
}

function parseArgs(argv) {
  const options = {
    repo: null,
    config: null,
    json: false,
    top: 15,
    baseline: null,
    updateBaseline: false,
    runTools: true,
    mutation: false,
    mutationOnly: false,
    section: null,
  };
  const values = new Map([
    ['--repo', 'repo'],
    ['--config', 'config'],
    ['--top', 'top'],
    ['--baseline', 'baseline'],
    ['--section', 'section'],
  ]);
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (values.has(arg)) {
      const value = argv[index + 1];
      if (!value || value.startsWith('--')) throw usageError(`${arg} requires a value`);
      options[values.get(arg)] = value;
      index += 1;
    } else if (arg === '--json') {
      options.json = true;
    } else if (arg === '--update-baseline') {
      options.updateBaseline = true;
    } else if (arg === '--no-run') {
      options.runTools = false;
    } else if (arg === '--mutation') {
      options.mutation = true;
    } else if (arg === '--mutation-only') {
      options.mutation = true;
      options.mutationOnly = true;
    } else {
      throw usageError(`unknown option: ${arg}`);
    }
  }
  options.top = Number(options.top);
  if (!Number.isInteger(options.top) || options.top < 1) throw usageError('--top must be a positive integer');
  return options;
}

function resolveFromRepo(repoRoot, value) {
  if (!value) return '';
  return isAbsolute(value) ? value : resolve(repoRoot, value);
}

function findRepoRoot(start) {
  let dir = resolve(start);
  while (true) {
    if (existsSync(join(dir, '.quality-gates.json'))) return dir;
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return resolve(start);
}

function readJson(path, label) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch (error) {
    throw usageError(`cannot read ${label} ${path}: ${error.message}`);
  }
}

function gitSha(repoRoot) {
  const result = spawnSync('git', ['-C', repoRoot, 'rev-parse', 'HEAD'], { encoding: 'utf8' });
  if (result.status !== 0) throw usageError(`cannot resolve git SHA for ${repoRoot}`);
  return result.stdout.trim();
}

const COVERAGE_FORMATS = new Set(['istanbul-json', 'llvm-cov-json']);
const COMPLEXITY_FORMATS = new Set(['eslint-json', 'rust-code-analysis-json']);

function validateAdapter(adapter, expectedFormats, label) {
  const formats = expectedFormats instanceof Set ? expectedFormats : new Set([expectedFormats]);
  if (!adapter || !formats.has(adapter.format) || !adapter.output || !adapter.command) {
    throw usageError(`${label} adapter must use ${[...formats].join(' or ')} and declare output`);
  }
}

function selectSection(config, section) {
  if (!section) return config;
  const nested = config[section];
  if (!nested || typeof nested !== 'object' || Array.isArray(nested)) {
    throw usageError(`unknown quality-gates section: ${section}`);
  }
  return {
    repo: nested.repo ?? config.repo,
    outputDir: nested.outputDir ?? config.outputDir,
    coverage: nested.coverage,
    complexity: nested.complexity,
    mutation: nested.mutation ?? config.mutation,
    thresholds: { ...(config.thresholds || {}), ...(nested.thresholds || {}) },
    baseline: nested.baseline ?? config.baseline,
  };
}

function isStrictDescendant(parent, child) {
  const rel = relative(resolve(parent), resolve(child)).replaceAll('\\', '/');
  return Boolean(rel) && rel !== '.' && !rel.startsWith('..');
}

function assertRustMetricsOutputPath(repoRoot, outputDir, outputPath) {
  if (!isStrictDescendant(repoRoot, outputDir)) {
    throw usageError('rust-code-analysis outputDir must be a dedicated child of the repository');
  }
  if (!isStrictDescendant(outputDir, outputPath)) {
    throw usageError('rust-code-analysis output must be a dedicated child of outputDir');
  }
}

function pathsOverlap(left, right) {
  const a = resolve(left);
  const b = resolve(right);
  return a === b || isStrictDescendant(a, b) || isStrictDescendant(b, a);
}

function assertRustMetricsDoesNotOverlapCoverage(repoRoot, outputDir, config) {
  if (config.complexity?.format !== 'rust-code-analysis-json') return;
  const metricsPath = configuredPath(repoRoot, outputDir, config.complexity.output);
  const coveragePath = configuredPath(repoRoot, outputDir, config.coverage.output);
  assertRustMetricsOutputPath(repoRoot, outputDir, metricsPath);
  if (pathsOverlap(metricsPath, coveragePath)) {
    throw usageError('rust-code-analysis output must not overlap the coverage output path');
  }
}

function listJsonFiles(dir) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch (error) {
    throw usageError(`cannot read complexity output ${dir}: ${error.message}`);
  }
  const files = [];
  for (const entry of entries) {
    if (entry.isSymbolicLink()) continue;
    const full = join(dir, entry.name);
    if (entry.isDirectory()) files.push(...listJsonFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.json')) files.push(full);
  }
  return files.sort((left, right) => left.localeCompare(right));
}

function rustSourceFileFromOutput(outputPath, jsonPath) {
  const rel = relative(outputPath, jsonPath).replaceAll('\\', '/');
  if (!rel || rel.startsWith('..') || isAbsolute(rel) || !rel.endsWith('.json')) {
    throw usageError(`cannot derive source path from rust-code-analysis output ${jsonPath}`);
  }
  return rel.slice(0, -'.json'.length);
}

function parseComplexityAdapter(repoRoot, outputDir, adapter) {
  const outputPath = configuredPath(repoRoot, outputDir, adapter.output);
  if (adapter.format !== 'rust-code-analysis-json') {
    return parseEslintComplexity(readJson(outputPath, 'complexity output'), repoRoot);
  }
  assertRustMetricsOutputPath(repoRoot, outputDir, outputPath);
  const jsonFiles = listJsonFiles(outputPath);
  if (!jsonFiles.length) {
    throw usageError(`complexity output ${outputPath} contains no json files`);
  }
  const functions = [];
  for (const jsonPath of jsonFiles) {
    const sourceFile = rustSourceFileFromOutput(outputPath, jsonPath);
    functions.push(...parseRustCodeAnalysisComplexity(
      readJson(jsonPath, 'complexity output'),
      repoRoot,
      sourceFile,
    ));
  }
  return functions;
}

function parseCoverageAdapter(repoRoot, outputDir, adapter) {
  const coverageJson = readJson(configuredPath(repoRoot, outputDir, adapter.output), 'coverage output');
  if (adapter.format === 'llvm-cov-json') {
    if (coverageJson?.type !== 'llvm.coverage.json.export') {
      throw usageError('coverage output is not llvm.coverage.json.export');
    }
    return parseLlvmCovCoverage(coverageJson, repoRoot);
  }
  return parseIstanbulCoverage(coverageJson, repoRoot);
}

function shellQuote(value) {
  return `'${String(value).replaceAll("'", "'\\''")}'`;
}

function configuredPath(repoRoot, outputDir, value) {
  return resolveFromRepo(repoRoot, String(value).replaceAll('{outputDir}', outputDir));
}

function writeChangedDiff(repoRoot, dest, sourceGlobs = []) {
  mkdirSync(dirname(dest), { recursive: true });
  const args = ['diff', '--unified=0', 'origin/main...HEAD', '--'];
  if (Array.isArray(sourceGlobs) && sourceGlobs.length) args.push(...sourceGlobs);
  const result = spawnSync('git', ['-C', repoRoot, ...args], {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw usageError(`cannot write changed diff: ${result.stderr || result.stdout || 'git diff failed'}`);
  }
  writeFileSync(dest, result.stdout || '');
  return Boolean((result.stdout || '').trim());
}

function runAdapter(repoRoot, outputDir, adapter, label, extras = {}) {
  let command = adapter.command.replaceAll('{outputDir}', shellQuote(outputDir));
  if (extras.changedDiff) {
    command = command.replaceAll('{changedDiff}', shellQuote(extras.changedDiff));
  }
  const outputPath = configuredPath(repoRoot, outputDir, adapter.output);
  if (adapter.format === 'rust-code-analysis-json') {
    assertRustMetricsOutputPath(repoRoot, outputDir, outputPath);
    rmSync(outputPath, { recursive: true, force: true });
    mkdirSync(outputPath, { recursive: true });
  } else {
    rmSync(outputPath, { force: true });
  }
  const inherit = adapter.format === 'llvm-cov-json' || adapter.format === 'rust-code-analysis-json';
  const result = spawnSync(command, {
    cwd: repoRoot,
    encoding: inherit ? undefined : 'utf8',
    shell: '/bin/sh',
    stdio: inherit ? ['ignore', 2, 2] : undefined,
    maxBuffer: 64 * 1024 * 1024,
  });
  if (!inherit) {
    if (result.stdout) process.stderr.write(result.stdout);
    if (result.stderr) process.stderr.write(result.stderr);
  }
  const outputExists = existsSync(outputPath);
  const allowedExit = result.status === 0
    || (adapter.format === 'eslint-json' && result.status === 1 && outputExists)
    || (adapter.format === 'istanbul-json' && result.status === 1 && outputExists)
    || (adapter.format === 'cargo-mutants-outcomes' && outputExists);
  if (result.error || !allowedExit) {
    throw usageError(`${label} command failed with exit ${result.status ?? 'unknown'}`);
  }
}

function main(argv) {
  const options = parseArgs(argv);
  const start = options.repo ? resolve(options.repo) : process.cwd();
  const repoRoot = findRepoRoot(start);
  const configPath = resolveFromRepo(repoRoot, options.config || '.quality-gates.json');
  const config = selectSection(readJson(configPath, 'config'), options.section);
  const outputDir = resolveFromRepo(repoRoot, config.outputDir || '.quality');
  mkdirSync(outputDir, { recursive: true });

  if (options.mutation) {
    validateAdapter(config.mutation, 'cargo-mutants-outcomes', 'mutation');
    const changedDiff = resolve(repoRoot, 'target/changes.diff');
    const hasDiff = writeChangedDiff(repoRoot, changedDiff, config.mutation.sourceGlobs);
    if (!hasDiff) {
      process.stdout.write('mutation: no matching rust changes vs origin/main...HEAD\n');
      if (options.mutationOnly) return 0;
    } else if (options.runTools) {
      runAdapter(repoRoot, outputDir, config.mutation, 'mutation', { changedDiff });
      const outcomes = readJson(
        configuredPath(repoRoot, outputDir, config.mutation.output),
        'mutation output',
      );
      const stats = parseCargoMutantsOutcomes(outcomes);
      const comparison = compareMutationThresholds(stats, config.thresholds || {});
      process.stdout.write(
        `mutation score ${stats.score} (caught ${stats.caught}/${stats.scored}; survivors ${stats.survivors})\n`,
      );
      for (const regression of comparison.regressions) {
        console.error(`${regression.metric}: current ${regression.current} vs required ${regression.baseline}`);
      }
      if (!comparison.passed) return 1;
      if (options.mutationOnly) return 0;
    } else if (options.mutationOnly) {
      return 0;
    }
  }

  if (options.mutationOnly) return 0;

  validateAdapter(config.coverage, COVERAGE_FORMATS, 'coverage');
  validateAdapter(config.complexity, COMPLEXITY_FORMATS, 'complexity');
  assertRustMetricsDoesNotOverlapCoverage(repoRoot, outputDir, config);
  if (options.runTools) {
    runAdapter(repoRoot, outputDir, config.coverage, 'coverage');
    runAdapter(repoRoot, outputDir, config.complexity, 'complexity');
  }

  let complexity = parseComplexityAdapter(repoRoot, outputDir, config.complexity);
  let coverage = parseCoverageAdapter(repoRoot, outputDir, config.coverage);
  if (config.coverage.format === 'llvm-cov-json') {
    complexity = filterProductionRustFunctions(complexity);
    coverage = filterProductionRustFunctions(coverage);
  }
  const joined = joinFunctionMetrics(complexity, coverage);
  const report = buildQualityReport({
    repo: config.repo || basename(repoRoot),
    sha: gitSha(repoRoot),
    generatedAt: new Date().toISOString(),
    functions: joined.functions,
    eslintOnly: joined.eslintOnly,
    crapFail: Number(config.thresholds?.crapFail ?? 30),
  });
  writeFileSync(join(outputDir, 'quality-report.json'), `${JSON.stringify(report, null, 2)}\n`);
  if (options.json) {
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } else {
    process.stdout.write(formatQualityMarkdown(report, options.top));
  }
  const baselinePath = resolveFromRepo(repoRoot, options.baseline || config.baseline);
  if (options.updateBaseline) {
    if (!baselinePath) throw usageError('--update-baseline requires --baseline or config.baseline');
    mkdirSync(dirname(baselinePath), { recursive: true });
    writeFileSync(baselinePath, `${JSON.stringify(buildRatchetBaseline(report), null, 2)}\n`);
    return 0;
  }
  if (baselinePath) {
    const baseline = readJson(baselinePath, 'baseline');
    if (baseline.crapFail !== report.totals.crapFail) {
      throw usageError(`baseline crapFail ${baseline.crapFail} does not match configured ${report.totals.crapFail}`);
    }
    const comparison = compareQualityRatchet(report, baseline);
    for (const regression of comparison.regressions) {
      console.error(`${regression.metric}: ${regression.current} > baseline ${regression.baseline}`);
    }
    return comparison.passed ? 0 : 1;
  }
  return report.totals.crapAboveFail > 0 ? 1 : 0;
}

try {
  process.exitCode = main(process.argv.slice(2));
} catch (error) {
  console.error(`repo-quality-check: ${error.message || error}`);
  process.exitCode = error.exitCode || 2;
}
