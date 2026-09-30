# Re-score the frozen baseline (the v0.1.1 resolver, commit 8d20102) with the current
# harness. Run after any scoring change so the "delta vs baseline" stays apples to apples.
# The baseline commit is checked out in a scratch worktree with this bench/ copied in; it
# reuses the corpus and SCIP ground truth of .cache/bench through junctions, with its own
# graph databases.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot\..").Path
$rev = '8d20102'
$wt = Join-Path ([IO.Path]::GetTempPath()) "codegraph-rebaseline"
$cache = Join-Path $root '.cache\bench-base'
$out = Join-Path $root 'bench\baseline\accuracy.json'

if (Test-Path $wt) { git -C $root worktree remove --force $wt }
git -C $root worktree add --detach $wt $rev | Out-Null
try {
    robocopy (Join-Path $root 'bench') (Join-Path $wt 'bench') /E /XD target /NFL /NDL /NJH /NJS | Out-Null
    New-Item -ItemType Directory -Force $cache | Out-Null
    foreach ($d in 'corpus', 'scip') {
        $link = Join-Path $cache $d
        if (-not (Test-Path $link)) {
            New-Item -ItemType Junction -Path $link -Target (Join-Path $root ".cache\bench\$d") | Out-Null
        }
    }
    $env:CODEGRAPH_BENCH_CACHE = $cache
    Push-Location (Join-Path $wt 'bench')
    try {
        cargo run --release -q -- accuracy --out $out
        if ($LASTEXITCODE -ne 0) { throw "accuracy run failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
} finally {
    # Junctions are removed as links only; the corpus they point at stays.
    foreach ($d in 'corpus', 'scip') {
        $link = Join-Path $cache $d
        if (Test-Path $link) { [IO.Directory]::Delete($link) }
    }
    git -C $root worktree remove --force $wt
}
Write-Host "wrote $out"
