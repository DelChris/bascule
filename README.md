# Bascule

**Bascule** — convertisseur de fichiers pour Windows, en Rust : une icône dans la zone de notification
et une entrée **« Convertir... »** au clic droit dans l'Explorateur.

| Fichier source | Actions proposées |
|---|---|
| HEIC / HEIF, AVIF, WebP, BMP, TIFF, JFIF | PNG, JPG, Réduire pour envoyer |
| PNG | JPG, Réduire pour envoyer |
| JPG / JPEG | PNG, Réduire pour envoyer |
| PDF | Compresser |

- **Réduire pour envoyer** : JPG dont le plus grand côté fait au plus 1920 px (qualité 80),
  pour un mail ou une messagerie. Fichier `photo (réduit).jpg`.
- **Compresser** (PDF) : les images du PDF sont ramenées à ~150 dpi et réencodées en JPG ;
  texte, polices et dessins vectoriels sont gardés tels quels. Fichier `doc (compressé).pdf`.
  Typiquement −40 à −80 % sur un PDF de photos ou de scans ; un PDF déjà compact est laissé tel quel.

## Installation

### Depuis une release

1. Pour les photos iPhone (HEIC), installer depuis le Microsoft Store **« Extensions d'images
   HEIF »** (gratuit) et **« Extensions vidéo HEVC »** (0,99 €, souvent déjà présente sur les PC
   récents). Pour l'AVIF : **« Extension vidéo AV1 »** (gratuit). Le reste fonctionne tel quel.
2. Télécharger `Bascule-x.y.z-x64.zip` dans les
   [releases](https://github.com/DelChris/bascule/releases), puis l'extraire.
3. Double-cliquer sur `bascule.exe`. SmartScreen avertit que l'exe n'est pas signé :
   « Informations complémentaires » → « Exécuter quand même ».
4. Accepter la demande administrateur, qui sert à faire confiance au certificat du package.

Le dossier extrait peut ensuite être supprimé : Bascule s'est copié dans
`%LOCALAPPDATA%\Programs\Bascule`.

### Depuis les sources

```bash
powershell -ExecutionPolicy Bypass -File packaging\build.ps1
target\release\bascule.exe --install
```

- `build.ps1` compile l'exe et la DLL d'extension, construit le package MSIX « sparse »
  (`examples/pack_msix.rs`, via l'API Windows) et le signe avec un certificat auto-signé
  `CN=Bascule` créé une fois dans le magasin de l'utilisateur.
- `--install` copie le tout dans `%LOCALAPPDATA%\Programs\Bascule`, active le lancement
  au démarrage et démarre le tray. Sous **Windows 11**, il enregistre le package :
  **« Convertir... » apparaît dans le menu contextuel principal**. La première fois, Windows
  demande une autorisation administrateur pour ajouter le certificat aux « Personnes
  autorisées » de la machine (sans quoi il refuse un package auto-signé).
- Si l'enregistrement échoue (UAC refusé, Windows 10…), l'installeur se rabat sur le menu
  classique (HKCU), visible dans « Afficher plus d'options » sous Windows 11.

Désinstallation : `bascule.exe --uninstall` (retire le package, les clés et le dossier ;
le certificat reste dans « Personnes autorisées », à retirer via `certlm.msc` si besoin).

Prérequis de compilation : toolchain Rust `stable-x86_64-pc-windows-gnu`.
`.cargo/config.toml` fait utiliser à `getrandom` (dépendance de `lopdf`) l'API RtlGenRandom,
qui évite d'avoir besoin de `dlltool`.

## Utilisation

- Sélectionner un ou plusieurs fichiers → clic droit → **Convertir...** → l'action voulue.
  Seules les actions qui s'appliquent à la sélection sont proposées. Le fichier est créé à côté
  de l'original (`photo.png`, ou `photo (1).png` s'il existe déjà).
- Une notification résume le lot ; un clic dessus ouvre l'Explorateur sur le fichier créé.
- Menu de l'icône : activer/désactiver le menu contextuel, le démarrage auto, la qualité JPG
  (75 → 100 %, 90 par défaut), quitter.

Ligne de commande (scripts, tests) :

```bash
bascule.exe --cli png photo.heic dossier\
bascule.exe --cli jpg dossier\
bascule.exe --cli reduce dossier\
bascule.exe --cli compress document.pdf
```

## Fonctionnement

```
Windows 11 : Explorateur ── bascule_shell.dll (IExplorerCommand, dans dllhost)
                               │  toute la sélection en UN message
Menu classique : Explorateur ──(1 process par fichier)── bascule.exe --to png "x.heic"
                               │  named pipe (~20 ms)
                               ▼
                tray résident : file de travaux → 4 workers
                WIC (HEIF, AV1, WebP, JPEG, TIFF…) → pixels → png (fdeflate) / JPG (WIC)
                PDF : lopdf → images réduites + réencodées en JPG (WIC) → réécriture compacte
```

- **Un exe** (~1,6 Mo) + **une DLL d'extension** (~360 Ko, `shell/`), aucune dépendance externe.
  Le package MSIX ne contient que le manifeste (`packaging/AppxManifest.xml`) et les logos :
  il pointe vers le dossier d'installation (`ExternalLocation`).
- L'extension n'apparaît que si la sélection contient un format pris en charge.
- Menu classique : les clients lancés en rafale sont regroupés en un seul lot (une notification).
  Si le tray ne tourne pas, il est démarré (par la DLL ou par le premier client).
- Tester la DLL sans rien installer :
  `cargo run --release -p bascule_shell --example test_dll -- png a.heic b.heic`.
- Le tray **préchauffe** le décodeur HEVC au démarrage (petit HEIC embarqué,
  `assets/warmup.heic`, régénérable via `cargo run --example make_warmup`).
- Profil couleur ICC conservé (Display P3 des iPhone). L'orientation est appliquée : par le
  décodeur de Windows pour le HEIC, d'après le tag EXIF pour le JPEG et le TIFF. Les
  métadonnées EXIF (date, GPS…) ne sont **pas** recopiées.
- Vers JPG, une image transparente (PNG, WebP…) est posée sur fond blanc.
- PDF : les masques de transparence, les images 1 bit, indexées, CMJN, JPEG 2000, CCITT et
  JBIG2 ne sont pas retouchés ; les PDF protégés sont refusés.
- Fichiers de test (PNG transparent, JPEG pivoté, BMP / TIFF, PDF lourd) :
  `cargo run --release --example make_samples -- <dossier> <grosse_photo.jpg>`.

### Mesures (Ryzen 5 7600X, photo 5712×3213 = 18 Mpx)

| | Temps |
|---|---|
| Décodage HEIC (WIC, chaud) | ~250 ms |
| Encodage PNG (`png` Fast + Paeth) | ~85 ms, 24,5 Mo |
| Encodage PNG via WIC (abandonné) | 1,3 s, 22,9 Mo |
| Encodage JPG q90 (WIC) | ~40 ms |
| Clic → fichier prêt (1 fichier, tray chaud) | ~0,25–0,6 s |
| 16 fichiers (simulation de l'Explorateur) | ~3,4 s en JPG, ~3,7 s en PNG |
| Réduire pour envoyer (HEIC 18 Mpx → 1920 px) | ~0,45 s |
| Compresser un PDF de 35 pages (12 Mo → 2,3 Mo) | ~0,26 s |

Variables d'environnement de diagnostic : `BASCULE_TRACE=1` (temps décodage/encodage
sur stderr), `BASCULE_THREADS=n`, `BASCULE_WARMUP=1` (mode `--cli`).

## Ajouter un format

1. une variante dans `Format` (`src/formats.rs`) et une ligne dans `SOURCES` ;
2. le traitement correspondant dans `src/convert.rs` (ou un module à part, comme `src/pdf.rs`) ;
3. pour une nouvelle extension source : un `<desktop5:ItemType>` dans `packaging/AppxManifest.xml`
   (et incrémenter `Version` du package).

## Suite possible

- Copie des métadonnées EXIF (date de prise de vue) vers le JPG.
- Décodeur HEIC plus rapide (libheif/libde265, ou HEVC matériel via Media Foundation).
- Images → PDF, PDF → images (`Windows.Data.Pdf`), vidéo iPhone → MP4 (Media Foundation).
