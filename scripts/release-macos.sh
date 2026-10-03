#!/usr/bin/env bash
# Synchrome — macOS release build (Tauri, embedded frontend, Developer ID signature, .app + .dmg).
#
# What it does, in order:
#   1. loads the two PUBLIC client values (SUPABASE_URL, SUPABASE_ANON_KEY) from the environment
#      or ./.env and bakes them into the binary (synchrome-config `option_env!`), so an installed
#      app needs no .env; administrative secrets have no compile-time counterpart at all;
#   2. strips absolute build paths from the binary (--remap-path-prefix) and symbols (profile);
#   3. builds the release bundle with `tauri build`, signed with the first valid
#      "Developer ID Application" identity of this Mac (override: APPLE_SIGNING_IDENTITY);
#      hardened runtime is Tauri's default;
#   4. notarizes + staples ONLY when notary credentials are present in the environment
#      (APPLE_API_KEY/APPLE_API_ISSUER/APPLE_API_KEY_PATH, or APPLE_ID/APPLE_PASSWORD/APPLE_TEAM_ID);
#      otherwise it says so and leaves an un-notarized signed bundle (Gatekeeper will refuse it
#      on other Macs until notarized);
#   5. verifies the bundle: codesign, embedded frontend (no devUrl), no dev-only code paths,
#      no .env, no developer paths, and prints the artefact hashes.
#
# Nothing here prints a secret. Run from the repository root:  scripts/release-macos.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD

if [[ -f .env ]]; then
  # publishable values only; .env is git-ignored and never bundled
  set -a; # shellcheck disable=SC1091
  source .env; set +a
fi
: "${SUPABASE_URL:?SUPABASE_URL (public) is required}"
: "${SUPABASE_ANON_KEY:?SUPABASE_ANON_KEY (public) is required}"
case "$SUPABASE_URL" in https://*) ;; *) echo "SUPABASE_URL must be https"; exit 1;; esac
# refuse to proceed if an administrative secret is in the build environment at all
for v in SUPABASE_SERVICE_ROLE_KEY SERVICE_ROLE_KEY JWT_SECRET POSTGRES_PASSWORD; do
  if [[ -n "${!v:-}" ]]; then echo "refusing to build with $v set in the environment"; exit 1; fi
done
export SUPABASE_URL SUPABASE_ANON_KEY

if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  APPLE_SIGNING_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -nE 's/^ *[0-9]+\) [0-9A-F]+ "(Developer ID Application: [^"]+)"$/\1/p' | head -1 || true)
fi
if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "no 'Developer ID Application' identity in the keychain: the bundle will be ad-hoc signed (local use only)"
  export APPLE_SIGNING_IDENTITY="-"
else
  export APPLE_SIGNING_IDENTITY
  echo "signing identity: ${APPLE_SIGNING_IDENTITY}"
fi

NOTARIZE=0
if [[ -n "${APPLE_API_KEY:-}" && -n "${APPLE_API_ISSUER:-}" && -n "${APPLE_API_KEY_PATH:-}" ]]; then NOTARIZE=1; fi
if [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then NOTARIZE=1; fi
if [[ $NOTARIZE == 1 ]]; then
  echo "notary credentials present: Tauri will notarize and staple"
else
  echo "no notary credentials in the environment: building a SIGNED, UN-NOTARIZED bundle"
  # make sure Tauri does not pick up partial settings
  unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH || true
fi

# no absolute developer paths in panic messages / debug info
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${HOME}=~ --remap-path-prefix=${ROOT}=."
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_PROFILE_RELEASE_DEBUG=0

# a previous interrupted DMG step can leave its read-write image mounted
for v in /Volumes/dmg.*; do [[ -d "$v" ]] && hdiutil detach "$v" -force -quiet || true; done
rm -f target/release/bundle/macos/rw.*.dmg

( cd apps/desktop && pnpm install --frozen-lockfile >/dev/null && pnpm tauri build --bundles app,dmg )

APP=$(ls -d target/release/bundle/macos/*.app | head -1)
DMG=$(ls target/release/bundle/dmg/*.dmg | head -1)
echo "app: $APP"
echo "dmg: $DMG"

echo "--- codesign"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -dv --verbose=2 "$APP" 2>&1 | grep -E "^(Authority|TeamIdentifier|Identifier|Runtime|Timestamp)" || true
echo "--- gatekeeper (expected to fail until notarized)"
spctl --assess --type execute --verbose=2 "$APP" || true

BIN="$APP/Contents/MacOS/$(defaults read "$ROOT/$APP/Contents/Info.plist" CFBundleExecutable)"
echo "--- bundle audit"
fail=0
check() { if [[ "$2" == "0" ]]; then echo "  ok   $1"; else echo "  FAIL $1 ($2 hits)"; fail=1; fi; }
check "no devUrl / localhost:5173 in the binary"   "$(strings "$BIN" | grep -c 'localhost:5173' || true)"
check "no dev CLI in the binary"                   "$(strings "$BIN" | grep -c -- '--dev-sign-in' || true)"
check "no dev UI driver in the binary"             "$(strings "$BIN" | grep -c 'SYNCHROME_DEV_UI_DRIVER' || true)"
check "no debug test hooks in the binary"          "$(strings "$BIN" | grep -cE 'SYNCHROME_DEV_(CORRUPT|TAMPER|REALTIME|FULL_REPLAY|COMPACTION|NO_AUTO)' || true)"
check "no SYNCHROME_DATA_DIR override"             "$(strings "$BIN" | grep -c 'SYNCHROME_DATA_DIR' || true)"
check "no developer home path"                     "$(strings "$BIN" | grep -c "$HOME" || true)"
check "no admin secret names"                      "$(strings "$BIN" | grep -cE 'SERVICE_ROLE|JWT_SECRET|POSTGRES_PASSWORD' || true)"
check "no .env inside the bundle"                  "$(find "$APP" -name '.env*' | wc -l | tr -d ' ')"
check "no test fixtures inside the bundle"         "$(find "$APP" -path '*tests/experiments*' | wc -l | tr -d ' ')"
[[ -f "$APP/Contents/Resources/extension/manifest.json" ]] && echo "  ok   extension manifest at Contents/Resources/extension" || { echo "  FAIL extension not bundled"; fail=1; }
# Tauri compresses the embedded frontend in release builds (invisible to `strings`); the build
# needs ../dist and the config has no devUrl, so the window can only load the embedded assets
# (confirmed at runtime by the clean-install smoke test).
if grep -q '"devUrl"' apps/desktop/src-tauri/tauri.conf.json; then echo "  FAIL devUrl present in tauri.conf.json"; fail=1; else echo "  ok   no devUrl in tauri.conf.json (embedded frontend only)"; fi
if [[ -f apps/desktop/dist/index.html ]]; then echo "  ok   frontend dist built and embedded"; else echo "  FAIL no frontend dist"; fail=1; fi
echo "--- artefacts"
shasum -a 256 "$BIN" "$DMG" | awk '{print "  " substr($1,1,16) "  " $2}'
du -sh "$APP" "$DMG" | awk '{print "  " $1 "  " $2}'
if [[ $fail == 1 ]]; then echo "BUNDLE AUDIT FAILED"; exit 1; fi
echo "release build OK (notarized: $NOTARIZE)"
