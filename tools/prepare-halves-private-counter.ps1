param([Parameter(Mandatory = $true)][string]$Checkout)
$ErrorActionPreference = 'Stop'
$target = (Resolve-Path -LiteralPath $Checkout).Path
$head = & git -C $target rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $head -ne '1b1ef2161a2f1cbcb46d7f72a7b24baf1303d419') {
    throw 'Use an isolated checkout of the exact official #2633 head 1b1ef216.'
}
$source = Split-Path -Parent $PSScriptRoot
$files = @(
    @('crates/victory-shader/src/private_lds.rs', 'crates/victory-shader/src/new354_private_lds.rs'),
    @('tools/halves-private-counter-repro.rs', 'crates/victory-shader/src/emit/new354_halves_private_counter_tests.rs')
)
foreach ($pair in $files) {
    $from = Join-Path $source $pair[0]
    $to = Join-Path $target $pair[1]
    if (Test-Path -LiteralPath $to) {
        if ((Get-FileHash -LiteralPath $from).Hash -ne (Get-FileHash -LiteralPath $to).Hash) {
            throw "Existing file differs: $to. Preserve it and use a fresh checkout."
        }
    } else {
        Copy-Item -LiteralPath $from -Destination $to
    }
}
$modules = @(
    @('crates/victory-shader/src/lib.rs', "`n#[cfg(test)]`nmod new354_private_lds;`n"),
    @('crates/victory-shader/src/emit/mod.rs', "`n#[cfg(all(test, windows))]`nmod new354_halves_private_counter_tests;`n")
)
foreach ($pair in $modules) {
    $file = Join-Path $target $pair[0]
    $text = [System.IO.File]::ReadAllText($file)
    $name = ($pair[1] -split "`n" | Where-Object { $_ -like 'mod *' })
    if (-not $text.Contains($name)) {
        [System.IO.File]::WriteAllText($file, $text + $pair[1], [System.Text.UTF8Encoding]::new($false))
    }
}
Write-Output 'Installed test-only proof and original/promoted synthetic comparison; production behavior unchanged.'
