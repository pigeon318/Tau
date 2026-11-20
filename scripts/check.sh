#!/usr/bin/env bash
# Everything the CI workflow checks, run on this machine (Linux), so that a change can be verified without
# GitHub Actions minutes:
#
#   scripts/check.sh            the full test suite, the VS Code extension tests (if node is installed) and a
#                               cross-build of the Windows tau.exe (if the Rust target and MinGW are installed)
#   scripts/check.sh quick      only the fast test binaries (about a minute: everything except the native, fuzz,
#                               robustness, git and update suites)
#   scripts/check.sh memory     also run the native tests under valgrind, UBSan and ThreadSanitizer
#
# The Windows tests themselves need a Windows machine: see scripts/windows-check.ps1.
set -u
cd "$(dirname "$0")/.."
mode="${1:-full}"
fail=0
step() { printf '\n== %s\n' "$1"; }
run() { if ! "$@"; then echo "FAILED: $*"; fail=1; fi; }
start=$(date +%s)

if [ "$mode" = quick ]; then
  step "quick tests"
  run cargo test --release --lib --test parser --test check --test check_examples --test borrowck --test borrow_examples --test errors --test examples --test modules --test programs --test run --test testrunner --test editor
else
  step "cargo test --release (the full suite: interpreter, native backend, robustness)"
  run cargo test --release
fi

if command -v node >/dev/null 2>&1 && [ -d editors/vscode ]; then
  step "VS Code extension (grammar and extension against the real compiler)"
  cargo build -q
  (cd editors/vscode && { [ -d node_modules ] || npm install --no-audit --no-fund >/dev/null 2>&1; } && run npm test)
else
  echo "(skipping the VS Code tests: node is not installed)"
fi

if rustup target list --installed 2>/dev/null | grep -q x86_64-pc-windows-gnu && command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
  step "cross-build tau.exe for Windows (catches code that only compiles on Linux)"
  run cargo build --release --target x86_64-pc-windows-gnu
else
  echo "(skipping the Windows cross-build: needs 'rustup target add x86_64-pc-windows-gnu' and mingw-w64)"
fi

if [ "$mode" = memory ]; then
  filter="tensor iterator shared mutex persistent slices integer comparisons_masks for_loops option_and_result clone_zero"
  step "native tests under valgrind"
  TAU_VALGRIND=1 run cargo test --release --test native -- $filter
  step "native tests under UBSan"
  TAU_CC=gcc TAU_SANITIZE=1 run cargo test --release --test native -- $filter
  step "native tests under ThreadSanitizer"
  TAU_CC=gcc TAU_SANITIZE=thread run cargo test --release --test native -- $filter
fi

echo
if [ "$fail" = 0 ]; then echo "ALL CHECKS PASSED in $(( $(date +%s) - start )) s"; else echo "SOME CHECKS FAILED"; fi
exit "$fail"
