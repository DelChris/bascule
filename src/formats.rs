//! Table des conversions possibles : extension source -> formats cibles.
//! Pour ajouter un type : une variante dans `Format` + une ligne dans `SOURCES`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpg,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpg),
            _ => None,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpg => "JPG",
        }
    }
}

/// Extensions sources (minuscules, avec le point) et formats proposés dans le menu.
pub const SOURCES: &[(&str, &[Format])] = &[
    (".heic", &[Format::Png, Format::Jpg]),
    (".heif", &[Format::Png, Format::Jpg]),
    (".hif", &[Format::Png, Format::Jpg]),
];
