# Shepherd checklist (merge gate)

Use this before merging a BusinessOS pull request. Hayes (or any Dueno shepherd) owns this gate. **No deploy** from the shepherd step unless Blaine explicitly authorizes one.

## Before merge

1. **Pull non-author reviews**
   - Fetch GitHub reviews and conversation/inline comments from accounts other than the PR author.
   - Include Dueno / autoReview bot reviews when present.
2. **Pull Dueno transcripts when GH is silent**
   - If Dueno reviewed in-session but did not post on the PR, pull those findings from the Dueno review session/transcript before merging.
3. **Clear or defer findings**
   - Every unanswered finding is either **addressed on the branch** or **explicitly deferred** with Blaine/Vera OK (linked issue or PR note).
4. **Do not treat Hayes-only LGTM as the full gate**
   - Especially when the GitHub identity is the PR author (self-approve is impossible; COMMENT LGTM is not a substitute for Dueno/autoReview).
5. **CI**
   - Required checks green (or explicitly waived by Blaine).

## After merge

- Close related Dueno fix/audit/review sessions (close-on-done) via Reid when applicable.
- Leave follow-ups as issues; do not deploy unless authorized.

## Why

PR #7 merged with Hayes COMMENT LGTM while Dueno findings lived only in session work (`0adf34be`), not as GitHub review comments. autoReview is being enabled so bot reviews should appear on PRs going forward — this checklist is the default merge gate either way.
