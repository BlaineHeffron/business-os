# Public Release Audit

The public tree came from one filtered export of the private source `HEAD`.

The export did not include Git history or forbidden configuration files.

The audit checks every file name and every text file before the initial commit.

The audit checks these categories without printing matched values:

- Client names and client code names
- Private operation names
- Email addresses
- Private hosts and deployment paths
- Tokens, keys, passwords, and credential assignments
- Private key headers
- Files and directories prohibited by the release task

The release result records only category counts and a pass or fail result.

## Result

The candidate tree audit passed on 2026-08-26.

The source revision was `03f0fe0eac172b105deb298c17473009c64d989f`.

The audit checked 1,166 candidate files.

The filename scan found no prohibited file, prohibited directory, private name, or client overlay path.

The content scan found no named client term, private operation term, private path, private host, or live business domain.

The domain-specific fixture scan found zero restricted fixture occurrences.

The connector scan found zero reserved-domain Google API scopes.

The credential test uses the public Gmail read-only scope constant.

The email scan found no unsafe address.

The email scan found 41 invented fixture addresses at public service domains.

The audit retained 10 public IP literals only in network security tests.

The audit found eight token-shaped values in synthetic security tests.

The audit confirmed that all eight values occur only after a test configuration boundary.

The audit found four credential assignment literals in test files.

The audit classified these literals as invented test data.

The audit found no private key material.

The package lock audit ignored checksum text and checked all semantic JSON fields.

The package lock audit found no private semantic value.

The audit excluded ignored build output and installed packages from the candidate file set.

The audit printed category counts only. It did not print matched values.

The Docker engine was unavailable. The audit did not build the generic container image.

The Git index audit checked 1,167 staged entries before the initial commit.

The Git index audit found zero prohibited entries and passed.
