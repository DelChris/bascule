# Convertisseur

Convertisseur de fichiers pour Windows, en Rust : une icône dans la zone de notification
et une entrée **« Convertir... »** au clic droit dans l'Explorateur.

POC : **HEIC / HEIF → PNG ou JPG**.

## Installation

```bash
powershell -ExecutionPolicy Bypass -File packaging\build.ps1
target\release\convertisseur.exe --install
```

- `build.ps1` compile l'exe et la DLL d'extension, construit le package MSIX « sparse »
  (`examples/pack_msix.rs`, via l'API Windows) et le signe avec un certificat auto-signé
  `CN=Convertisseur` créé une fois dans le magasin de l'utilisateur.
- `--install` copie le tout dans `%LOCALAPPDATA%\Programs\Convertisseur`, active le lancement
  au démarrage et démarre le tray. Sous **Windows 11**, il enregistre le package :
  **« Convertir... » apparaît dans le menu contextuel principal**. La première fois, Windows
  demande une autorisation administrateur pour ajouter le certificat aux « Personnes
  autorisées » de la machine (sans quoi il refuse un package auto-signé).
- Si l'enregistrement échoue (UAC refusé, Windows 10…), l'installeur se rabat sur le menu
  classique (HKCU), visible dans « Afficher plus d'options » sous Windows 11.

Désinstallation : `convertisseur.exe --uninstall` (retire le package, les clés et le dossier ;
le certificat reste dans « Personnes autorisées », à retirer via `certlm.msc` si besoin).

Prérequis : les extensions Microsoft Store **« Extensions d'images HEIF »** et
**« Extensions vidéo HEVC »** (décodage HEIC natif de Windows).

## Utilisation

- Sélectionner un ou plusieurs `.heic` → clic droit → **Convertir...** → **PNG** / **JPG**.
  Le fichier est créé à côté de l'original (`photo.png`, ou `photo (1).png` s'il existe déjà).
- Une notification résume le lot ; un clic dessus ouvre l'Explorateur sur le fichier créé.
- Menu de l'icône : activer/désactiver le menu contextuel, le démarrage auto, la qualité JPG
  (75 → 100 %, 90 par défaut), quitter.

Ligne de commande (scripts, tests) :

```bash
convertisseur.exe --cli png photo.heic dossier\
convertisseur.exe --cli jpg dossier\
```

## Fonctionnement

```
Windows 11 : Explorateur ── convertisseur_shell.dll (IExplorerCommand, dans dllhost)
                               │  toute la sélection en UN message
Menu classique : Explorateur ──(1 process par fichier)── convertisseur.exe --to png "x.heic"
                               │  named pipe (~20 ms)
                               ▼
                tray résident : file de travaux → 4 workers
                WIC (HEIF/HEVC) → pixels → png (fdeflate) / JPG (WIC)
```

- **Un exe** (~660 Ko) + **une DLL d'extension** (~360 Ko, `shell/`), aucune dépendance externe.
  Le package MSIX ne contient que le manifeste (`packaging/AppxManifest.xml`) et les logos :
  il pointe vers le dossier d'installation (`ExternalLocation`).
- L'extension n'apparaît que si la sélection contient un format pris en charge.
- Menu classique : les clients lancés en rafale sont regroupés en un seul lot (une notification).
  Si le tray ne tourne pas, il est démarré (par la DLL ou par le premier client).
- Tester la DLL sans rien installer :
  `cargo run --release -p convertisseur_shell --example test_dll -- png a.heic b.heic`.
- Le tray **préchauffe** le décodeur HEVC au démarrage (petit HEIC embarqué,
  `assets/warmup.heic`, régénérable via `cargo run --example make_warmup`).
- Profil couleur ICC conservé (Display P3 des iPhone). L'orientation est appliquée par le
  décodeur de Windows. Les métadonnées EXIF (date, GPS…) ne sont **pas** recopiées.

### Mesures (Ryzen 5 7600X, photo 5712×3213 = 18 Mpx)

| | Temps |
|---|---|
| Décodage HEIC (WIC, chaud) | ~250 ms |
| Encodage PNG (`png` Fast + Paeth) | ~85 ms, 24,5 Mo |
| Encodage PNG via WIC (abandonné) | 1,3 s, 22,9 Mo |
| Encodage JPG q90 (WIC) | ~40 ms |
| Clic → fichier prêt (1 fichier, tray chaud) | ~0,25–0,6 s |
| 16 fichiers (simulation de l'Explorateur) | ~3,4 s en JPG, ~3,7 s en PNG |

Variables d'environnement de diagnostic : `CONVERTISSEUR_TRACE=1` (temps décodage/encodage
sur stderr), `CONVERTISSEUR_THREADS=n`, `CONVERTISSEUR_WARMUP=1` (mode `--cli`).

## Ajouter un format

1. une variante dans `Format` (`src/formats.rs`) et une ligne dans `SOURCES` ;
2. l'encodage correspondant dans `src/convert.rs` ;
3. pour une nouvelle extension source : un `<desktop5:ItemType>` dans `packaging/AppxManifest.xml`
   (et incrémenter `Version` du package).

## Suite possible

- Copie des métadonnées EXIF vers le JPG.
- Décodeur HEIC plus rapide (libheif/libde265, ou HEVC matériel via Media Foundation).
- Autres formats (WebP, AVIF, PDF → images, vidéo…).
