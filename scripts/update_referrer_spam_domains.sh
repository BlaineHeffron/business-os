#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

source_url="${REFERRER_SPAM_LIST_URL:-https://raw.githubusercontent.com/matomo-org/referrer-spam-list/master/spammers.txt}"
target="data/referrer-spam-domains.txt"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

curl -fsSL "$source_url" |
  sed -E 's/#.*$//' |
  tr '[:upper:]' '[:lower:]' |
  sed -E 's#^https?://##; s#/.*$##; s/[[:space:]]+//g; s/^\.+//; s/\.+$//' |
  awk 'length($0) > 0' |
  LC_ALL=C sort -u > "$tmp"

mkdir -p "$(dirname "$target")"
{
  echo "# Referrer-spam domains vendored from Matomo's public-domain community list."
  echo "# Source: $source_url"
  echo "# Update with: just referrer-spam-domains"
  echo "# Do not hand-edit; add client-specific overrides in overlay/env config."
  cat "$tmp"
} > "$target"

echo "$target refreshed ($(wc -l < "$tmp") domains)"
