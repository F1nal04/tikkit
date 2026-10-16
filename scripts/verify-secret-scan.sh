#!/usr/bin/env bash
# Prove that the gitleaks configuration catches provider API keys and redacts
# them, without committing anything that looks like a credential.
#
# Fake keys are assembled at runtime from split prefixes plus random
# characters inside a throwaway directory outside the repository, then
# scanned with the repository's .gitleaks.toml. The script fails if any
# provider rule does not fire or if a fake key appears in gitleaks output.
#
# Usage: scripts/verify-secret-scan.sh   (requires gitleaks on PATH)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

rand() { LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c "$1" || true; }

# Prefixes are split so this file never matches the rules itself.
openai_project="sk""-proj-TIKKITFAKE$(rand 40)"
openai_legacy="sk""-TIKKITFAKE$(rand 38)"
anthropic="sk""-ant-api03-TIKKITFAKE$(rand 80)"
google="AI""za$(rand 35)"

mkdir -p "$work/fixture"
cat >"$work/fixture/provider-keys.env" <<KEYS
OPENAI_API_KEY=$openai_project
OPENAI_LEGACY_KEY=$openai_legacy
ANTHROPIC_API_KEY=$anthropic
GEMINI_API_KEY=$google
KEYS

set +e
gitleaks dir --no-banner --redact --verbose \
  --config "$repo_root/.gitleaks.toml" \
  --report-format json --report-path "$work/report.json" \
  "$work/fixture" >"$work/output.txt" 2>&1
status=$?
set -e

fail=0
if [ "$status" -eq 0 ]; then
  echo "FAIL: gitleaks exited 0 on a fixture that contains fake provider keys" >&2
  fail=1
fi
for rule in tikkit-openai-project-key tikkit-openai-legacy-key tikkit-anthropic-key tikkit-google-api-key; do
  if ! grep -q "\"RuleID\": \"$rule\"" "$work/report.json"; then
    echo "FAIL: rule $rule did not fire" >&2
    fail=1
  fi
done
for key in "$openai_project" "$openai_legacy" "$anthropic" "$google"; do
  if grep -qF "$key" "$work/output.txt" "$work/report.json"; then
    echo "FAIL: a fake key was printed unredacted" >&2
    fail=1
  fi
done

if [ "$fail" -ne 0 ]; then
  cat "$work/output.txt" >&2
  exit 1
fi
echo "OK: gitleaks flagged all fake provider keys (exit $status) and redacted them"
