//! Icône dessinée en code (pas de fichier .rc / windres) : carré arrondi bleu + deux flèches.

use std::path::PathBuf;

const SIZES: [u32; 7] = [16, 20, 24, 32, 48, 64, 256];

/// Écrit l'icône dans le dossier d'installation (référencée par le menu contextuel).
pub fn ensure_file() -> PathBuf {
    let dir = crate::util::install_dir();
    let path = dir.join("bascule.ico");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, ico_file());
    path
}

/// Logo PNG (pour le package MSIX).
pub fn write_png(path: &std::path::Path, s: u32) -> std::io::Result<()> {
    let mut px = render(s);
    for p in px.chunks_mut(4) {
        p.swap(0, 2); // BGRA -> RGBA
    }
    let mut enc = png::Encoder::new(std::io::BufWriter::new(std::fs::File::create(path)?), s, s);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(std::io::Error::other)?;
    w.write_image_data(&px).map_err(std::io::Error::other)?;
    w.finish().map_err(std::io::Error::other)
}

/// Fichier .ico complet (plusieurs tailles), utilisé par le menu contextuel.
pub fn ico_file() -> Vec<u8> {
    let images: Vec<Vec<u8>> = SIZES.iter().map(|&s| dib(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * SIZES.len();
    for (&s, img) in SIZES.iter().zip(&images) {
        let b = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[b, b, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        out.extend(img);
    }
    out
}

/// Image d'icône au format DIB (BITMAPINFOHEADER + BGRA bas-en-haut + masque AND).
pub fn dib(s: u32) -> Vec<u8> {
    let px = render(s);
    let row = (s * 4) as usize;
    let mask_row = (s.div_ceil(32) * 4) as usize;
    let mut v = Vec::with_capacity(40 + px.len() + mask_row * s as usize);
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&(s as i32).to_le_bytes());
    v.extend_from_slice(&((2 * s) as i32).to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&((px.len() + mask_row * s as usize) as u32).to_le_bytes());
    v.extend_from_slice(&[0u8; 16]);
    for y in (0..s as usize).rev() {
        v.extend_from_slice(&px[y * row..(y + 1) * row]);
    }
    v.resize(v.len() + mask_row * s as usize, 0);
    v
}

fn in_tri(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let e = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| (p.0 - r.0) * (q.1 - r.1) - (q.0 - r.0) * (p.1 - r.1);
    let (d1, d2, d3) = (e(p, a, b), e(p, b, c), e(p, c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

fn in_rect(p: (f32, f32), x0: f32, x1: f32, y0: f32, y1: f32) -> bool {
    p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1
}

fn in_round_rect(p: (f32, f32), inset: f32, r: f32) -> bool {
    let half = 0.5 - inset;
    let dx = ((p.0 - 0.5).abs() - (half - r)).max(0.0);
    let dy = ((p.1 - 0.5).abs() - (half - r)).max(0.0);
    dx * dx + dy * dy <= r * r
}

fn arrows(p: (f32, f32)) -> bool {
    // flèche du haut vers la droite, flèche du bas vers la gauche
    in_rect(p, 0.22, 0.60, 0.31, 0.43)
        || in_tri(p, (0.56, 0.19), (0.56, 0.55), (0.81, 0.37))
        || in_rect(p, 0.40, 0.78, 0.57, 0.69)
        || in_tri(p, (0.44, 0.45), (0.44, 0.81), (0.19, 0.63))
}

/// Pixels BGRA (alpha non prémultiplié), haut-en-bas, suréchantillonnés 4x4.
fn render(s: u32) -> Vec<u8> {
    const N: u32 = 4;
    let inset = if s <= 20 { 0.0 } else { 0.03 };
    let mut out = Vec::with_capacity((s * s * 4) as usize);
    for y in 0..s {
        for x in 0..s {
            let (mut bg, mut fg) = (0u32, 0u32);
            for sy in 0..N {
                for sx in 0..N {
                    let p = (
                        (x as f32 + (sx as f32 + 0.5) / N as f32) / s as f32,
                        (y as f32 + (sy as f32 + 0.5) / N as f32) / s as f32,
                    );
                    if in_round_rect(p, inset, 0.22) {
                        bg += 1;
                        if arrows(p) {
                            fg += 1;
                        }
                    }
                }
            }
            let a = bg as f32 / (N * N) as f32;
            let f = if bg > 0 { fg as f32 / bg as f32 } else { 0.0 };
            // dégradé #3B82F6 (haut) -> #1D4ED8 (bas)
            let t = y as f32 / (s - 1).max(1) as f32;
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            let (r, g, b) = (lerp(59.0, 29.0), lerp(130.0, 78.0), lerp(246.0, 216.0));
            let mix = |c: f32| (c + (255.0 - c) * f).round() as u8;
            out.extend_from_slice(&[mix(b), mix(g), mix(r), (a * 255.0).round() as u8]);
        }
    }
    out
}
