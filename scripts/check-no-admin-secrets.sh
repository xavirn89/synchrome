#!/usr/bin/env bash
# Fails if any administrative secret NAME appears in the product tree (code, frontend, bundle,
# client config). Experiment scripts under tests/experiments are intentionally excluded: they
# use an admin key only for server-side test setup/teardown and never ship.
set -euo pipefail
cd "$(dirname "$0")/.."
PATTERN='SUPABASE_SERVICE_ROLE_KEY|SERVICE_ROLE|JWT_SECRET|POSTGRES_PASSWORD'
TARGETS=(crates apps .env.example README.md docs cloud)
hits=$(grep -rnE "$PATTERN" "${TARGETS[@]}" 2>/dev/null \
  | grep -v node_modules | grep -v '/target/' \
  | grep -vE 'docs/.*(must never|never (reach|ship)|not .* in the (client|binary)|no service_role)' || true)
if [[ -n "$hits" ]]; then
  echo "FORBIDDEN admin-secret references in product tree:"; echo "$hits"; exit 1
fi
echo "OK: no administrative secret names in crates/, apps/, cloud/, docs/, README, .env.example"
