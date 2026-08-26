import tsParser from '@typescript-eslint/parser';

export default [
  {
    ignores: [
      'coverage/**',
      'dist/**',
      'src/**/*.test.ts',
      'src/**/*.test.tsx',
      'src/types/generated/**',
    ],
  },
  {
    files: ['src/**/*.ts', 'src/**/*.tsx'],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        ecmaFeatures: { jsx: true },
        ecmaVersion: 'latest',
        sourceType: 'module',
      },
    },
    rules: {
      // max 0 is a measurement trick: every function is reported so CRAP can
      // join complexity with coverage. Do not copy this into a real lint job.
      complexity: ['error', { max: 0 }],
    },
  },
];
