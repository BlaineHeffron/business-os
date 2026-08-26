import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import {
  filterProductionRustFunctions,
  joinFunctionMetrics,
  parseLlvmCovCoverage,
  parseRustCodeAnalysisComplexity,
} from '../repo-quality.mjs';

const fixtureDir = join(dirname(fileURLToPath(import.meta.url)), 'fixtures');
const readJsonFixture = (name) => JSON.parse(readFileSync(join(fixtureDir, name), 'utf8'));

describe('rust CRAP adapters', () => {
  it('counts only same-file llvm-cov code regions, earliest start, and 0-based columns', () => {
    assert.deepEqual(parseLlvmCovCoverage(readJsonFixture('llvm-cov.json'), '/repo'), [
      {
        file: 'src/cart.rs',
        line: 10,
        column: 2,
        endLine: 13,
        name: 'checkout',
        statementsTotal: 3,
        statementsCovered: 2,
        called: true,
      },
    ]);
  });

  it('subtracts nested rust-code-analysis spaces from parent cyclomatic', () => {
    assert.deepEqual(
      parseRustCodeAnalysisComplexity(readJsonFixture('rust-code-analysis.json'), '/repo', 'src/cart.rs'),
      [
        { file: 'src/cart.rs', line: 10, column: 0, cc: 12, name: 'checkout' },
        { file: 'src/cart.rs', line: 10, column: 0, cc: 4, name: 'closure' },
        { file: 'src/cart.rs', line: 50, column: 0, cc: 9, name: 'helper' },
      ],
    );
  });

  it('prefers rust-code-analysis root name when it normalizes inside the repo', () => {
    assert.deepEqual(
      parseRustCodeAnalysisComplexity(readJsonFixture('rust-code-analysis.json'), '/repo', 'abs/src/cart.rs'),
      [
        { file: 'src/cart.rs', line: 10, column: 0, cc: 12, name: 'checkout' },
        { file: 'src/cart.rs', line: 10, column: 0, cc: 4, name: 'closure' },
        { file: 'src/cart.rs', line: 50, column: 0, cc: 9, name: 'helper' },
      ],
    );
  });

  it('drops rust-code-analysis functions whose direct cyclomatic is below 1', () => {
    assert.deepEqual(
      parseRustCodeAnalysisComplexity({
        name: '/repo/src/cart.rs',
        kind: 'unit',
        spaces: [{
          name: 'broken',
          start_line: 3,
          kind: 'function',
          metrics: { cyclomatic: { sum: 2 } },
          spaces: [{ metrics: { cyclomatic: { sum: 4 } }, spaces: [] }],
        }],
      }, '/repo', 'src/cart.rs'),
      [],
    );
  });

  it('prefers compatible names when two complexity functions share a start line', () => {
    const joined = joinFunctionMetrics(
      [
        { file: 'src/nested.rs', line: 10, column: 0, cc: 12, name: 'parent' },
        { file: 'src/nested.rs', line: 10, column: 0, cc: 4, name: 'closure' },
      ],
      [
        {
          file: 'src/nested.rs', line: 10, column: 8, endLine: 12, name: 'nested::closure',
          statementsTotal: 1, statementsCovered: 0, called: false,
        },
        {
          file: 'src/nested.rs', line: 10, column: 0, endLine: 20, name: 'nested::parent',
          statementsTotal: 2, statementsCovered: 1, called: true,
        },
      ],
    );
    assert.equal(joined.functions.find((fn) => fn.name.includes('closure')).cc, 4);
    assert.equal(joined.functions.find((fn) => fn.name.includes('parent')).cc, 12);
  });

  it('joins llvm-cov and rust-code-analysis functions by file and start line', () => {
    const complexity = parseRustCodeAnalysisComplexity(
      readJsonFixture('join-rust-code-analysis.json'),
      '/repo',
      'src/nested.rs',
    );
    const coverage = parseLlvmCovCoverage(readJsonFixture('join-llvm-cov.json'), '/repo');

    assert.deepEqual(joinFunctionMetrics(complexity, coverage), {
      functions: [
        {
          file: 'src/nested.rs', line: 14, column: 0, endLine: 16, name: 'closure',
          statementsTotal: 1, statementsCovered: 0, called: false,
          cc: 4, coverage: 0, crap: 20,
        },
        {
          file: 'src/nested.rs', line: 10, column: 0, endLine: 20, name: 'parent',
          statementsTotal: 2, statementsCovered: 1, called: true,
          cc: 12, coverage: 0.5, crap: 30,
        },
        {
          file: 'src/nested.rs', line: 30, column: 0, endLine: 32, name: 'unmatched',
          statementsTotal: 1, statementsCovered: 1, called: true,
          cc: null, coverage: 1, crap: null,
        },
      ],
      eslintOnly: [
        { file: 'src/nested.rs', line: 50, column: 0, cc: 9, name: 'rustOnly' },
      ],
    });
  });

  it('keeps production crates/ and apps/ functions and drops target, registry, and tests', () => {
    const kept = filterProductionRustFunctions([
      { file: 'crates/bos-app/src/lib.rs', name: 'prod' },
      { file: 'apps/bos-server/src/main.rs', name: 'bin' },
      { file: 'crates/bos-app/src/slices/work_queue/tests.rs', name: 'slice_tests' },
      { file: 'crates/bos-kernel/src/tests/helpers.rs', name: 'test_helper' },
      { file: 'target/debug/build/foo/out/lib.rs', name: 'build_script' },
      { file: '/usr/local/cargo/registry/src/index.crates.io/serde-1.0.0/src/lib.rs', name: 'serde' },
      { file: 'frontend/src/lib.ts', name: 'ts' },
    ]);
    assert.deepEqual(kept.map((fn) => fn.name), ['prod', 'bin']);
  });
});
