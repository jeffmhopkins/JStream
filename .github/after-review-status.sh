#!/usr/bin/env bash
# Posts this after-review job's state as a commit status named after its check (CONTEXT),
# so a dispatched run counts on the PR (after-review.yml). RESULT is pending or job.status.
set -euo pipefail
case "$RESULT" in
  pending | success) state="$RESULT" ;;
  cancelled) state=error ;;
  *) state=failure ;;
esac
gh api --silent "repos/$GITHUB_REPOSITORY/statuses/$GITHUB_SHA" \
  -f state="$state" -f context="$CONTEXT" -f description="after-review: $RESULT" \
  -f target_url="$GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID"
