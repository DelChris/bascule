# Compile l'exe + la DLL, construit le package MSIX sparse et le signe.
# Produit dans target\release : convertisseur.exe, convertisseur_shell.dll,
# Convertisseur.msix, convertisseur.cer
#
#   powershell -ExecutionPolicy Bypass -File packaging\build.ps1
#
# Outil : signtool du Windows SDK. Sans SDK installé, le script prend celui du
# paquet NuGet officiel Microsoft.Windows.SDK.BuildTools (extrait dans %LOCALAPPDATA%\SdkBuildTools).

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'target\release'
$subject = 'CN=Convertisseur'   # doit correspondre à Publisher dans AppxManifest.xml

function Find-Tool($name) {
    $dirs = @("${env:ProgramFiles(x86)}\Windows Kits\10\bin", "$env:LOCALAPPDATA\SdkBuildTools")
    $t = Get-ChildItem $dirs -Recurse -Filter $name -ErrorAction SilentlyContinue |
        Where-Object FullName -match '\\x64\\' | Sort-Object FullName | Select-Object -Last 1
    if (-not $t) {
        $v = (Invoke-RestMethod https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.buildtools/index.json).versions |
            Where-Object { $_ -notmatch '-' } | Select-Object -Last 1
        $d = "$env:LOCALAPPDATA\SdkBuildTools\$v"
        New-Item -ItemType Directory -Force $d | Out-Null
        Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.buildtools/$v/microsoft.windows.sdk.buildtools.$v.nupkg" -OutFile "$d\pkg.zip"
        Expand-Archive "$d\pkg.zip" "$d\x" -Force
        $t = Get-ChildItem $d -Recurse -Filter $name | Where-Object FullName -match '\\x64\\' | Select-Object -First 1
    }
    if ((Get-AuthenticodeSignature $t.FullName).Status -ne 'Valid') { throw "$name : signature invalide" }
    $t.FullName
}

Push-Location $root
try {
    cargo build --release --workspace
    if ($LASTEXITCODE) { throw 'cargo build a échoué' }

    # Contenu du package : manifeste + logos
    $stage = Join-Path $root 'target\package'
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force "$stage\Assets" | Out-Null
    Copy-Item "$PSScriptRoot\AppxManifest.xml" $stage
    & "$out\convertisseur.exe" --export-assets "$stage\Assets" | Out-Null
    Start-Sleep -Milliseconds 300   # exe GUI : rend la main avant d'avoir fini d'écrire
    Copy-Item "$stage\Assets\convertisseur.ico" $out

    # Empaquetage via l'API Windows (examples/pack_msix.rs) : pas besoin de makeappx.
    Remove-Item "$stage\Assets\convertisseur.ico"
    $msix = Join-Path $out 'Convertisseur.msix'
    cargo run --release --quiet --example pack_msix -- $stage $msix
    if ($LASTEXITCODE) { throw "l'empaquetage MSIX a échoué" }

    # Certificat de signature auto-signé (magasin de l'utilisateur, créé une seule fois)
    $cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $subject -and $_.HasPrivateKey } |
        Sort-Object NotAfter | Select-Object -Last 1
    if (-not $cert) {
        $cert = New-SelfSignedCertificate -Type Custom -Subject $subject -KeyUsage DigitalSignature `
            -FriendlyName 'Convertisseur (signature locale)' -CertStoreLocation Cert:\CurrentUser\My `
            -NotAfter (Get-Date).AddYears(10) `
            -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
    }
    Export-Certificate -Cert $cert -FilePath (Join-Path $out 'convertisseur.cer') | Out-Null

    & (Find-Tool 'signtool.exe') sign /q /fd SHA256 /sha1 $cert.Thumbprint /s My $msix
    if ($LASTEXITCODE) { throw 'signtool a échoué' }

    Write-Host "OK : $msix (certificat $($cert.Thumbprint))"
}
finally { Pop-Location }
