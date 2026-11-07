//! Table des conversions possibles : extension source -> actions proposées.
//! Pour ajouter un type : une variante dans `Format` + une ligne dans `SOURCES`.
//! Partagé avec la DLL du menu contextuel (pas de dépendance à `windows` ici).

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpg,
    /// JPG redimensionné (côté le plus long ≤ `REDUCE_MAX_SIDE`) pour mail / messagerie.
    Reduce,
    /// PDF dont les images sont recompressées.
    Compress,
}

/// Côté le plus long d'une image « réduite pour envoyer ».
pub const REDUCE_MAX_SIDE: u32 = 1920;
/// Qualité JPG des images réduites.
pub const REDUCE_QUALITY: u32 = 80;

impl Format {
    #[allow(dead_code)] // utilisé par la DLL
    pub const ALL: [Format; 4] = [Format::Png, Format::Jpg, Format::Reduce, Format::Compress];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpg),
            "reduce" | "reduire" => Some(Self::Reduce),
            "compress" | "compresser" => Some(Self::Compress),
            _ => None,
        }
    }

    /// Identifiant pour la ligne de commande, le pipe et le registre.
    pub fn id(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Reduce => "reduce",
            Self::Compress => "compress",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpg => "JPG",
            Self::Reduce => "Réduire pour envoyer",
            Self::Compress => "Compresser",
        }
    }

    /// Extension du fichier produit.
    pub fn out_ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg | Self::Reduce => "jpg",
            Self::Compress => "pdf",
        }
    }

    /// Suffixe ajouté au nom (le fichier produit garde sinon le nom de l'original).
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Png | Self::Jpg => "",
            Self::Reduce => " (réduit)",
            Self::Compress => " (compressé)",
        }
    }
}

use Format::*;

const PHOTO: &[Format] = &[Png, Jpg, Reduce];

/// Extensions sources (minuscules, avec le point) et actions proposées dans le menu.
pub const SOURCES: &[(&str, &[Format])] = &[
    (".heic", PHOTO),
    (".heif", PHOTO),
    (".hif", PHOTO),
    (".avif", PHOTO),
    (".webp", PHOTO),
    (".png", &[Jpg, Reduce]),
    (".jpg", &[Png, Reduce]),
    (".jpeg", &[Png, Reduce]),
    (".jfif", &[Png, Jpg, Reduce]),
    (".bmp", PHOTO),
    (".tif", PHOTO),
    (".tiff", PHOTO),
    (".pdf", &[Compress]),
];

/// Actions proposées pour ce fichier (vide s'il n'est pas pris en charge).
pub fn targets(path: &Path) -> &'static [Format] {
    let Some(ext) = path.extension() else { return &[] };
    let ext = format!(".{}", ext.to_string_lossy().to_lowercase());
    SOURCES.iter().find(|(e, _)| *e == ext).map_or(&[], |(_, t)| *t)
}

pub fn accepts(path: &Path, fmt: Format) -> bool {
    targets(path).contains(&fmt)
}
