# The Windows part of the CI workflow, run on your own Windows PC (so it costs no Actions minutes).
#
# Needs: Rust (https://rustup.rs) and a C compiler on the PATH (MinGW-w64 gcc, the same one `tau` uses).
# Run it from the repository folder:   powershell -ExecutionPolicy Bypass -File scripts\windows-check.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

Write-Host "== toolchain"
gcc --version | Select-Object -First 1
rustc --version

Write-Host "== cargo test --release"
cargo test --release
if ($LASTEXITCODE -ne 0) { Write-Error "the test suite failed"; exit 1 }

Write-Host "== stack overflow handling, repeated (it used to fail intermittently)"
1..10 | ForEach-Object {
    cargo test --release --test native -- runaway_recursion 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Write-Error "run $_ failed"; exit 1 }
}

Write-Host "== build tau.exe and smoke test it"
cargo build --release
.\target\release\tau.exe --version
foreach ($name in "collatz", "primes", "sort", "bank", "wordcount", "integrate") {
    $out = (.\target\release\tau.exe run "examples\run\$name.tau" --native) -join "`n"
    $want = (Get-Content "examples\run\$name.expected" -Raw).Trim()
    if ($out.Trim() -ne $want) { Write-Error "$name differs"; exit 1 }
    Write-Host "$name ok"
}
# no C compiler: falls back to the interpreter with a note
$env:TAU_CC = "no-such-compiler"
.\target\release\tau.exe run examples\hello.tau
if ($LASTEXITCODE -ne 0) { exit 1 }
Remove-Item Env:\TAU_CC
.\target\release\tau.exe clean

Write-Host ""
Write-Host "ALL WINDOWS CHECKS PASSED"
