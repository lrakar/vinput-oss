# Builds a release of Vinput and writes what the site and the update check need.
#
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1
#
# What is new comes from releases.json at the root of the repository, which
# must have an entry for the version in the root Cargo.toml ([workspace.package]).
# The same entry feeds the
# update window of the app and the release notes page of the site.
#
# Output, under dist\releases:
#   VinputSetup-<version>.exe   the setup program (Vinput itself, under the name that makes it install)
#   latest.json                 the manifest the running app reads to find this release
#   history.json                every release, for the release notes page
#
# Publishing is a separate, deliberate step: copy the files to site\releases
# on the `site` branch and deploy the site. Until latest.json is live, no
# installed copy sees the release. See docs\release.md.
param(
    # Where the release will be served from.
    [string]$BaseUrl = "https://vinput-dasvcr.web.app/releases"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# The version of every crate is the one under [workspace.package] in the root
# Cargo.toml; cargo is asked, so this cannot disagree with what gets built.
$metadata = cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
$version = ($metadata.packages | Where-Object { $_.name -eq "vinput" }).version
if (-not $version) { throw "no version found in Cargo.toml" }

$history = Get-Content releases.json -Raw -Encoding UTF8 | ConvertFrom-Json
$entry = $history | Where-Object { $_.version -eq $version } | Select-Object -First 1
if (-not $entry) { throw "releases.json has no entry for version $version; add one first" }

# Built into its own folder, so a copy of Vinput that is running from
# target\release does not hold the output file.
$target = Join-Path $root "target\dist"
# .cargo\config.toml links the C runtime into the executable; a RUSTFLAGS
# variable left over in this shell would replace those flags.
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
cargo build --release --locked -p vinput --target-dir $target
if ($LASTEXITCODE -ne 0) { throw "the build failed" }
$built = Join-Path $target "release\vinput.exe"

# A setup program has to start on a PC that has nothing but Windows.
$image = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($built))
if ($image.IndexOf("VCRUNTIME140", [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
    throw "the executable needs the Visual C++ runtime; see .cargo\config.toml"
}

$out = Join-Path $root "dist\releases"
New-Item -ItemType Directory -Force $out | Out-Null
$name = "VinputSetup-$version.exe"
$setup = Join-Path $out $name
Copy-Item $built $setup -Force

$manifest = [ordered]@{
    version   = $version
    notes     = @($entry.notes)
    url       = "$BaseUrl/$name"
    sha256    = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
    bytes     = (Get-Item $setup).Length
    published = $entry.published
}
# Written without a byte-order mark, which the app does not expect.
$utf8 = New-Object System.Text.UTF8Encoding $false
$json = $manifest | ConvertTo-Json
[System.IO.File]::WriteAllText((Join-Path $out "latest.json"), $json, $utf8)
[System.IO.File]::WriteAllText((Join-Path $out "history.json"), (Get-Content releases.json -Raw -Encoding UTF8), $utf8)

Write-Output "Built $setup"
Write-Output $json
