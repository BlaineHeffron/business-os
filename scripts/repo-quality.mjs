import { isAbsolute, relative, resolve } from 'node:path';

function normalizeFile(filePath, repoRoot) {
  const absolute = resolve(filePath);
  const fromRoot = relative(resolve(repoRoot), absolute);
  return fromRoot && !fromRoot.startsWith('..') && !isAbsolute(fromRoot)
    ? fromRoot
    : absolute;
}

function complexityName(message) {
  const subject = String(message).split(' has a complexity of ')[0];
  const named = /^(?:async )?(?:function|method) ['"](.+)['"]$/i.exec(subject);
  return named?.[1] || subject;
}

export function parseEslintComplexity(eslintJson, repoRoot) {
  const functions = [];
  for (const result of Array.isArray(eslintJson) ? eslintJson : []) {
    for (const message of Array.isArray(result?.messages) ? result.messages : []) {
      if (message.ruleId !== 'complexity') continue;
      const cc = Number(/complexity of (\d+)/.exec(String(message.message))?.[1]);
      if (!Number.isFinite(cc)) continue;
      functions.push({
        file: normalizeFile(result.filePath, repoRoot),
        line: Number(message.line),
        column: Math.max(0, Number(message.column || 1) - 1),
        cc,
        name: complexityName(message.message),
      });
    }
  }
  return functions;
}

function positionAtOrAfter(position, start) {
  return position.line > start.line
    || (position.line === start.line && position.column >= start.column);
}

function positionAtOrBefore(position, end) {
  return position.line < end.line
    || (position.line === end.line && position.column <= end.column);
}

export function parseIstanbulCoverage(coverageJson, repoRoot) {
  const functions = [];
  for (const [filePath, fileCoverage] of Object.entries(coverageJson || {})) {
    const statements = Object.entries(fileCoverage?.statementMap || {}).map(([id, location]) => ({
      location,
      covered: Number(fileCoverage?.s?.[id] || 0) > 0,
    }));
    for (const [id, fn] of Object.entries(fileCoverage?.fnMap || {})) {
      const body = fn.loc;
      const inside = statements.filter(({ location }) => (
        positionAtOrAfter(location.start, body.start)
        && positionAtOrBefore(location.end, body.end)
      ));
      functions.push({
        file: normalizeFile(filePath, repoRoot),
        line: Number(fn.decl.start.line),
        column: Number(fn.decl.start.column),
        endLine: Number(body.end.line),
        name: fn.name,
        statementsTotal: inside.length,
        statementsCovered: inside.filter(({ covered }) => covered).length,
        called: Number(fileCoverage?.f?.[id] || 0) > 0,
      });
    }
  }
  return functions;
}

const LLVM_CODE_REGION = 0;

export function parseLlvmCovCoverage(coverageJson, repoRoot) {
  const functions = [];
  for (const exportData of Array.isArray(coverageJson?.data) ? coverageJson.data : []) {
    for (const fn of Array.isArray(exportData?.functions) ? exportData.functions : []) {
      const filenames = Array.isArray(fn?.filenames) ? fn.filenames : [];
      const codeRegions = (Array.isArray(fn?.regions) ? fn.regions : []).filter((region) => (
        Array.isArray(region)
        && region.length >= 8
        && Number(region[7]) === LLVM_CODE_REGION
      ));
      if (!codeRegions.length) continue;
      const fileId = Number(codeRegions[0][5]);
      const filePath = filenames[fileId];
      if (!filePath) continue;
      const sameFile = codeRegions.filter((region) => Number(region[5]) === fileId);
      const start = [...sameFile].sort((left, right) => {
        const lineDelta = Number(left[0]) - Number(right[0]);
        return lineDelta !== 0 ? lineDelta : Number(left[1]) - Number(right[1]);
      })[0];
      functions.push({
        file: normalizeFile(filePath, repoRoot),
        line: Number(start[0]),
        column: Math.max(0, Number(start[1] || 1) - 1),
        endLine: Math.max(...sameFile.map((region) => Number(region[2]))),
        name: String(fn.name || ''),
        statementsTotal: sameFile.length,
        statementsCovered: sameFile.filter((region) => Number(region[4]) > 0).length,
        called: Number(fn.count || 0) > 0,
      });
    }
  }
  return functions;
}

function rustCyclomaticSum(space) {
  return Number(space?.metrics?.cyclomatic?.sum);
}

function collectRustFunctions(space, file, functions) {
  if (!space || typeof space !== 'object') return;
  const children = Array.isArray(space.spaces) ? space.spaces : [];
  if (space.kind === 'function') {
    const sum = rustCyclomaticSum(space);
    const childSum = children.reduce((total, child) => {
      const value = rustCyclomaticSum(child);
      return total + (Number.isFinite(value) ? value : 0);
    }, 0);
    const cc = sum - childSum;
    if (Number.isFinite(cc) && cc >= 1) {
      functions.push({
        file,
        line: Number(space.start_line),
        column: 0,
        cc,
        name: String(space.name || ''),
      });
    }
  }
  for (const child of children) collectRustFunctions(child, file, functions);
}

export function parseRustCodeAnalysisComplexity(funcSpace, repoRoot, sourceFile) {
  const functions = [];
  const fromName = typeof funcSpace?.name === 'string' && funcSpace.name
    ? normalizeFile(funcSpace.name, repoRoot)
    : '';
  const fromOutput = normalizeFile(resolve(repoRoot, sourceFile), repoRoot);
  const file = fromName && !isAbsolute(fromName) ? fromName : fromOutput;
  collectRustFunctions(funcSpace, file, functions);
  return functions;
}

const PRODUCTION_RUST_ROOTS = ['crates', 'apps'];
const TEST_RUST_PATH = /(^|\/)tests\.rs$|(^|\/)tests\//;

export function isProductionRustFile(filePath) {
  const file = String(filePath || '').replaceAll('\\', '/');
  if (TEST_RUST_PATH.test(file)) return false;
  return PRODUCTION_RUST_ROOTS.some((root) => file === root || file.startsWith(`${root}/`));
}

export function filterProductionRustFunctions(functions) {
  return (Array.isArray(functions) ? functions : []).filter((fn) => isProductionRustFile(fn?.file));
}

function namesCompatible(left, right) {
  const a = String(left || '');
  const b = String(right || '');
  if (!a || !b) return false;
  return a === b || a.endsWith(b) || b.endsWith(a) || a.includes(b) || b.includes(a);
}

export function joinFunctionMetrics(complexityFunctions, coverageFunctions) {
  const complexity = (Array.isArray(complexityFunctions) ? complexityFunctions : [])
    .map((fn, index) => ({ ...fn, index }));
  const used = new Set();
  const functions = (Array.isArray(coverageFunctions) ? coverageFunctions : []).map((fn) => {
    const sameSite = complexity.filter((candidate) => (
      !used.has(candidate.index)
      && candidate.file === fn.file
      && candidate.line === fn.line
    ));
    const named = sameSite.filter((candidate) => namesCompatible(candidate.name, fn.name));
    const pool = named.length ? named : sameSite;
    const match = [...pool].sort((left, right) => (
      Math.abs(left.column - fn.column) - Math.abs(right.column - fn.column)
    ))[0];
    if (match) used.add(match.index);
    const coverage = fn.statementsTotal > 0
      ? fn.statementsCovered / fn.statementsTotal
      : (fn.called ? 1 : 0);
    const crap = match
      ? match.cc ** 2 * (1 - coverage) ** 3 + match.cc
      : null;
    return {
      ...fn,
      name: match?.name || fn.name,
      cc: match?.cc ?? null,
      coverage,
      crap,
    };
  });
  const eslintOnly = complexity
    .filter((fn) => !used.has(fn.index))
    .map(({ index, ...fn }) => fn);
  return { functions, eslintOnly };
}

function round(value, digits = 4) {
  const factor = 10 ** digits;
  return Math.round((value + Number.EPSILON) * factor) / factor;
}

export function buildQualityReport({
  repo,
  sha,
  generatedAt,
  functions = [],
  eslintOnly = [],
  crapFail = 30,
}) {
  const ordered = [...functions].sort((left, right) => {
    const crapDelta = (right.crap ?? Number.NEGATIVE_INFINITY)
      - (left.crap ?? Number.NEGATIVE_INFINITY);
    if (crapDelta) return crapDelta;
    return left.file.localeCompare(right.file)
      || left.line - right.line
      || left.column - right.column;
  });
  const scored = ordered.filter((fn) => Number.isFinite(fn.crap));
  const aboveFail = scored.filter((fn) => fn.crap > crapFail);
  const statementsTotal = ordered.reduce((sum, fn) => sum + fn.statementsTotal, 0);
  const statementsCovered = ordered.reduce((sum, fn) => sum + fn.statementsCovered, 0);
  return {
    repo,
    sha,
    generatedAt,
    totals: {
      functions: ordered.length,
      scoredFunctions: scored.length,
      unmatchedComplexity: ordered.length - scored.length, // coverage fns with no ESLint CC
      eslintOnly: eslintOnly.length,
      statementsTotal,
      statementsCovered,
      statementCoverage: statementsTotal ? round(statementsCovered / statementsTotal) : 0,
      calledFunctions: ordered.filter((fn) => fn.called).length,
      uncalledFunctions: ordered.filter((fn) => !fn.called).length,
      crapFail,
      crapAboveFail: aboveFail.length,
      crapSumAboveFail: round(aboveFail.reduce((sum, fn) => sum + fn.crap, 0), 2),
      crapAbove6: scored.filter((fn) => fn.crap > 6).length,
    },
    functions: ordered,
  };
}

export function buildRatchetBaseline(report) {
  return {
    repo: report.repo,
    sha: report.sha,
    generatedAt: report.generatedAt,
    crapFail: report.totals.crapFail,
    crapAboveFail: report.totals.crapAboveFail,
    crapSumAboveFail: report.totals.crapSumAboveFail,
  };
}

const SUM_EPSILON = 0.01;

export function validateRatchetBaseline(baseline) {
  const crapFail = Number(baseline?.crapFail);
  const crapAboveFail = Number(baseline?.crapAboveFail);
  const crapSumAboveFail = Number(baseline?.crapSumAboveFail);
  if (!Number.isFinite(crapFail) || crapFail < 0) {
    throw new Error('baseline crapFail must be a finite number >= 0');
  }
  if (!Number.isInteger(crapAboveFail) || crapAboveFail < 0) {
    throw new Error('baseline crapAboveFail must be an integer >= 0');
  }
  if (!Number.isFinite(crapSumAboveFail) || crapSumAboveFail < 0) {
    throw new Error('baseline crapSumAboveFail must be a finite number >= 0');
  }
  return { crapFail, crapAboveFail, crapSumAboveFail };
}

export function compareQualityRatchet(report, baseline) {
  const allowed = validateRatchetBaseline(baseline);
  const current = {
    crapAboveFail: report.totals.crapAboveFail,
    crapSumAboveFail: report.totals.crapSumAboveFail,
  };
  const regressions = [];
  if (current.crapAboveFail > allowed.crapAboveFail) {
    regressions.push({
      metric: 'crapAboveFail',
      baseline: allowed.crapAboveFail,
      current: current.crapAboveFail,
    });
  }
  if (current.crapSumAboveFail > allowed.crapSumAboveFail + SUM_EPSILON) {
    regressions.push({
      metric: 'crapSumAboveFail',
      baseline: allowed.crapSumAboveFail,
      current: current.crapSumAboveFail,
    });
  }
  return {
    passed: regressions.length === 0,
    current,
    baseline: {
      crapAboveFail: allowed.crapAboveFail,
      crapSumAboveFail: allowed.crapSumAboveFail,
    },
    regressions,
  };
}

function outcomeSummary(entry) {
  if (typeof entry === 'string') return entry;
  const raw = entry?.summary ?? entry?.outcome ?? entry?.result;
  if (typeof raw === 'string') return raw;
  if (raw && typeof raw === 'object') {
    const key = Object.keys(raw)[0];
    if (key) return key;
  }
  return '';
}

function isMutantScenario(entry) {
  const scenario = entry?.scenario;
  if (scenario == null) return true;
  if (typeof scenario === 'string') {
    return !/^baseline$/i.test(scenario);
  }
  if (typeof scenario === 'object') {
    if (scenario.Mutant || scenario.mutant) return true;
    if (scenario.Baseline || scenario.baseline) return false;
  }
  return true;
}

function classifyMutantSummary(summary) {
  const value = String(summary || '').toLowerCase();
  if (!value) return 'other';
  if (value.includes('unviable') || value.includes('unmutated')) return 'unviable';
  if (value.includes('caught') || value === 'killed') return 'caught';
  if (value.includes('missed') || value.includes('surviv')) return 'missed';
  if (value.includes('timeout')) return 'timeout';
  if (value.includes('check') || value.includes('fail')) return 'failed';
  return 'other';
}

export function parseCargoMutantsOutcomes(outcomesJson) {
  const list = Array.isArray(outcomesJson)
    ? outcomesJson
    : (Array.isArray(outcomesJson?.outcomes) ? outcomesJson.outcomes : []);
  const stats = {
    mutants: 0,
    caught: 0,
    missed: 0,
    timeout: 0,
    failed: 0,
    unviable: 0,
    other: 0,
  };
  for (const entry of list) {
    if (!isMutantScenario(entry)) continue;
    const bucket = classifyMutantSummary(outcomeSummary(entry));
    if (bucket === 'unviable') {
      stats.unviable += 1;
      continue;
    }
    stats.mutants += 1;
    stats[bucket] += 1;
  }
  const scored = stats.caught + stats.missed + stats.timeout + stats.failed;
  const score = scored ? stats.caught / scored : 1;
  return {
    ...stats,
    scored,
    score: scored ? round(score) : 1,
    survivors: stats.missed + stats.timeout + stats.failed,
  };
}

export function compareMutationThresholds(stats, thresholds = {}) {
  const minScore = thresholds.mutationScoreMinChanged;
  const maxSurvivors = thresholds.survivorsMaxChanged;
  const regressions = [];
  if (Number.isFinite(Number(minScore)) && stats.scored > 0 && stats.score + Number.EPSILON < Number(minScore)) {
    regressions.push({
      metric: 'mutationScoreMinChanged',
      baseline: Number(minScore),
      current: stats.score,
    });
  }
  if (maxSurvivors != null && Number.isFinite(Number(maxSurvivors)) && stats.survivors > Number(maxSurvivors)) {
    regressions.push({
      metric: 'survivorsMaxChanged',
      baseline: Number(maxSurvivors),
      current: stats.survivors,
    });
  }
  return { passed: regressions.length === 0, regressions, stats };
}

export function formatQualityMarkdown(report, top = 15) {
  const rows = report.functions
    .filter((fn) => Number.isFinite(fn.crap))
    .slice(0, top)
    .map((fn) => {
      const name = String(fn.name || '(anonymous)').replaceAll('|', '\\|');
      return `| ${fn.crap.toFixed(2)} | ${fn.cc} | ${(fn.coverage * 100).toFixed(1)}% | ${fn.statementsCovered}/${fn.statementsTotal} | \`${fn.file}:${fn.line}\` ${name} |`;
    });
  return [
    `## CRAP report: ${report.repo}`,
    '',
    '| CRAP | CC | Coverage | Statements | Function |',
    '| ---: | ---: | ---: | ---: | --- |',
    ...rows,
    '',
    `Functions: ${report.totals.functions}; CRAP > ${report.totals.crapFail}: ${report.totals.crapAboveFail}; CRAP > 6: ${report.totals.crapAbove6}; statements: ${(report.totals.statementCoverage * 100).toFixed(1)}%.`,
    '',
  ].join('\n');
}
