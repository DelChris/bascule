# Enregistre / retire le package sparse (menu contextuel Windows 11).
# Appelé par `convertisseur.exe --install / --uninstall` (embarqué dans l'exe).
param(
    [Parameter(Mandatory)][ValidateSet('install', 'uninstall')] [string]$Action,
    [Parameter(Mandatory)] [string]$Dir
)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
trap { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }

Get-AppxPackage -Name 'Convertisseur' | Remove-AppxPackage
if ($Action -eq 'uninstall') { exit 0 }

$cer = Join-Path $Dir 'convertisseur.cer'
$msix = Join-Path $Dir 'Convertisseur.msix'
$cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 $cer

# Windows n'installe un package que si sa signature est approuvée : le certificat
# auto-signé va dans « Personnes autorisées » de la machine (une seule fois, droits admin).
if (-not (Test-Path "Cert:\LocalMachine\TrustedPeople\$($cert.Thumbprint)")) {
    $cmd = "Import-Certificate -FilePath '$cer' -CertStoreLocation Cert:\LocalMachine\TrustedPeople"
    try {
        Start-Process powershell.exe -Verb RunAs -Wait -WindowStyle Hidden `
            -ArgumentList '-NoProfile', '-Command', $cmd
    } catch {
        throw "Autorisation administrateur refusée : le certificat n'a pas pu être approuvé."
    }
    if (-not (Test-Path "Cert:\LocalMachine\TrustedPeople\$($cert.Thumbprint)")) {
        throw "Le certificat n'a pas été ajouté aux Personnes autorisées."
    }
}

Add-AppxPackage -Path $msix -ExternalLocation $Dir
