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
#   latest.json                 the manifest the running app reads to find this release, signed
#   history.json                every release, for the release notes page
#
# Publishing is a separate, deliberate step: copy the files to site\releases
# on the `site` branch and deploy the site. Until latest.json is live, no
# installed copy sees the release. See docs\release.md.
#
# The manifest is signed with the publisher's key, which is a file kept
# outside the repository and never on the site: installed copies take an
# update only with that signature (crates\putlab-core\src\update.rs). Whoever
# has the file can publish updates, and without it nobody can, so it is kept
# safe and it is backed up. It is never printed.
param(
    # Where the release will be served from.
    [string]$BaseUrl = "https://vinput-dasvcr.web.app/releases",
    # The publisher's key. PUTLAB_SIGNING_KEY names another file.
    [string]$SigningKey = $(if ($env:PUTLAB_SIGNING_KEY) { $env:PUTLAB_SIGNING_KEY } else { Join-Path $env:USERPROFILE ".vinput-cloud\update-signing.pk8" })
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# Asked for before anything is built: a release nobody can sign is no release.
if (-not (Test-Path -LiteralPath $SigningKey -PathType Leaf)) {
    throw "the publisher's key is not at $SigningKey; set PUTLAB_SIGNING_KEY or pass -SigningKey"
}

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
# The address of the VinPUT Pro server is built in from VINPUT_SERVER; the
# source names none (cratesputlab-coresrcoud.rs).
if (-not $env:VINPUT_SERVER) { throw "set VINPUT_SERVER to the address of the VinPUT Pro server" }

# The compiler writes the path of every source file it was given into the
# messages of the executable, and those paths are under the home folder of
# whoever builds it. Each folder is written under a short name instead, so a
# release says nothing about the PC it was built on. The flags go in a
# configuration file of their own, which cargo adds to the ones in
# .cargo\config.toml; the last name that fits a path is the one used.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$names = [ordered]@{
    $env:USERPROFILE          = "home"
    (rustc --print sysroot)   = "rust"
    $cargoHome                = "cargo"
    $root                     = "vinput"
}
$flags = $names.GetEnumerator() | ForEach-Object {
    $flag = "--remap-path-prefix=$($_.Key)=$($_.Value)"
    '"' + $flag.Replace('\', '\\').Replace('"', '\"') + '"'
}
New-Item -ItemType Directory -Force $target | Out-Null
$paths = Join-Path $target "paths.toml"
$lines = "[target.x86_64-pc-windows-msvc]", "rustflags = [$($flags -join ', ')]"
[System.IO.File]::WriteAllLines($paths, $lines)

cargo build --release --locked -p vinput --target-dir $target --config $paths
if ($LASTEXITCODE -ne 0) { throw "the build failed" }
$built = Join-Path $target "release\vinput.exe"

# A setup program has to start on a PC that has nothing but Windows.
$image = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($built))
if ($image.IndexOf("VCRUNTIME140", [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
    throw "the executable needs the Visual C++ runtime; see .cargo\config.toml"
}
# Nor may it name the home folder of whoever built it.
if ($image.IndexOf($env:USERPROFILE, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
    throw "the executable names $env:USERPROFILE; see the path names above"
}

$out = Join-Path $root "dist\releases"
New-Item -ItemType Directory -Force $out | Out-Null
$name = "VinputSetup-$version.exe"
$setup = Join-Path $out $name
Copy-Item $built $setup -Force

$sha256 = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
$bytes = (Get-Item $setup).Length

# The signature is over whose setup program this is, its version, its
# checksum and its size; the tool that makes it is the one whose words the
# app checks it against. Its output is the signature and nothing else.
$signature = cargo run --quiet --locked -p putlab-core --example sign --target-dir $target -- release $SigningKey VinputSetup $version $sha256 $bytes
if ($LASTEXITCODE -ne 0 -or -not $signature) { throw "signing the release failed" }

$manifest = [ordered]@{
    version   = $version
    notes     = @($entry.notes)
    url       = "$BaseUrl/$name"
    sha256    = $sha256
    bytes     = $bytes
    published = $entry.published
    signature = "$signature".Trim()
}
# Written without a byte-order mark, which the app does not expect.
$utf8 = New-Object System.Text.UTF8Encoding $false
$json = $manifest | ConvertTo-Json
[System.IO.File]::WriteAllText((Join-Path $out "latest.json"), $json, $utf8)
[System.IO.File]::WriteAllText((Join-Path $out "history.json"), (Get-Content releases.json -Raw -Encoding UTF8), $utf8)

Write-Output "Built $setup"
Write-Output $json
