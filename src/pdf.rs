//! « Compresser » un PDF : ce sont presque toujours les images qui pèsent (scans, photos).
//! Chaque image est réduite (côté le plus long ≤ `MAX_SIDE`, ~150 dpi sur une page A4)
//! et réencodée en JPG ; elle n'est remplacée que si le gain est réel. Le reste du fichier
//! (texte, polices, vectoriel) est conservé tel quel, puis tout est réécrit avec des flux
//! d'objets compressés.
//!
//! Laissés intacts : masques (transparence), images 1 bit / indexées / CMJN, JPEG 2000,
//! CCITT, JBIG2, et les PDF chiffrés.

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use windows::core::Interface;
use windows::Win32::Graphics::Imaging::*;

use crate::convert::{trace, Converter, Error, Pixels};

/// Côté le plus long des images (1754 px = A4 à 150 dpi).
const MAX_SIDE: u32 = 1754;
const QUALITY: u32 = 75;
/// En dessous, une image ne vaut pas la peine d'être retouchée.
const MIN_BYTES: usize = 16 * 1024;
/// Une image n'est remplacée que si elle maigrit d'au moins 10 %.
const MIN_GAIN: f64 = 0.9;
/// Garde-fou contre les « bombes » de décompression.
const MAX_DECODED: usize = 512 * 1024 * 1024;

pub(crate) fn compress(conv: &Converter, src: &Path, out: &Path) -> Result<(), Error> {
    let t = Instant::now();
    let data = std::fs::read(src).map_err(|e| format!("lecture : {e}"))?;
    let mut doc = Document::load_mem(&data).map_err(|e| format!("PDF illisible : {e}"))?;
    if doc.was_encrypted() || doc.is_encrypted() {
        return Err("PDF protégé : non pris en charge".to_string().into());
    }
    trace("pdf load", t);

    // Les masques de transparence restent intacts (un JPG y ferait des artefacts).
    let mut masks: HashSet<ObjectId> = HashSet::new();
    for obj in doc.objects.values() {
        if let Object::Stream(s) = obj {
            for key in [&b"SMask"[..], b"Mask"] {
                if let Ok(Object::Reference(id)) = s.dict.get(key) {
                    masks.insert(*id);
                }
            }
        }
    }
    let images: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(id, o)| !masks.contains(id) && matches!(o, Object::Stream(s) if is_image(&s.dict)))
        .map(|(id, _)| *id)
        .collect();

    let t = Instant::now();
    let mut replaced = 0;
    for id in images {
        let Ok(Object::Stream(s)) = doc.get_object(id) else { continue };
        let Some(comps) = components(&doc, &s.dict) else { continue };
        let Some(new) = (unsafe { recompress(conv, s, comps) }) else { continue };
        if let Ok(Object::Stream(s)) = doc.get_object_mut(id) {
            s.dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
            s.dict.remove(b"DecodeParms");
            s.dict.set("Width", new.w as i64);
            s.dict.set("Height", new.h as i64);
            s.dict.set("BitsPerComponent", 8);
            s.set_content(new.data);
            replaced += 1;
        }
    }
    trace(&format!("pdf images ({replaced} remplacées)"), t);

    let t = Instant::now();
    doc.prune_objects();
    doc.compress();
    let mut buf = Vec::with_capacity(data.len());
    doc.save_modern(&mut buf).map_err(|e| format!("écriture du PDF : {e}"))?;
    trace("pdf save", t);

    if buf.len() as f64 >= data.len() as f64 * 0.95 {
        return Err("PDF déjà compact : aucun gain possible".to_string().into());
    }
    std::fs::write(out, buf)?;
    Ok(())
}

fn is_image(d: &Dictionary) -> bool {
    let name_is = |k: &[u8], v: &[u8]| d.get(k).and_then(Object::as_name).is_ok_and(|n| n == v);
    name_is(b"Subtype", b"Image")
        && !d.get(b"ImageMask").and_then(Object::as_bool).unwrap_or(false)
        && d.get(b"Decode").is_err()
        && d.get(b"BitsPerComponent").and_then(Object::as_i64).is_ok_and(|b| b == 8)
}

/// Nombre de composantes (1 = gris, 3 = RGB) si l'espace colorimétrique est pris en charge.
fn components(doc: &Document, d: &Dictionary) -> Option<u32> {
    let cs = d.get(b"ColorSpace").ok()?;
    let (_, cs) = doc.dereference(cs).ok()?;
    match cs {
        Object::Name(n) => match n.as_slice() {
            b"DeviceRGB" => Some(3),
            b"DeviceGray" => Some(1),
            _ => None,
        },
        Object::Array(a) => match a.first()?.as_name().ok()? {
            b"CalRGB" => Some(3),
            b"CalGray" => Some(1),
            b"ICCBased" => {
                let (_, icc) = doc.dereference(a.get(1)?).ok()?;
                match icc.as_stream().ok()?.dict.get(b"N").and_then(Object::as_i64).ok()? {
                    1 => Some(1),
                    3 => Some(3),
                    _ => None,
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// Réduit / réencode une image. `None` : non prise en charge, ou pas assez de gain.
unsafe fn recompress(conv: &Converter, s: &Stream, comps: u32) -> Option<Pixels> {
    if s.content.len() < MIN_BYTES {
        return None;
    }
    let f = &conv.factory;
    let w = s.dict.get(b"Width").and_then(Object::as_i64).ok()? as u32;
    let h = s.dict.get(b"Height").and_then(Object::as_i64).ok()? as u32;
    let filters = s.filters().unwrap_or_default();
    let target = if comps == 1 { GUID_WICPixelFormat8bppGray } else { GUID_WICPixelFormat24bppBGR };

    let source: IWICBitmapSource = if filters == [&b"DCTDecode"[..]] {
        let stream = f.CreateStream().ok()?;
        stream.InitializeFromMemory(&s.content).ok()?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        // CMJN : l'inversion Adobe rend le résultat incertain, on n'y touche pas.
        if frame.GetPixelFormat().ok()? == GUID_WICPixelFormat32bppCMYK {
            return None;
        }
        frame.cast().ok()?
    } else {
        let decodable = [&b"FlateDecode"[..], b"LZWDecode", b"RunLengthDecode", b"ASCII85Decode", b"ASCIIHexDecode"];
        if !filters.iter().all(|f| decodable.contains(f)) {
            return None; // JPXDecode, CCITTFaxDecode, JBIG2Decode…
        }
        let raw = s.decompressed_content_with_limit(MAX_DECODED).ok()?;
        let stride = w * comps;
        if raw.len() < stride as usize * h as usize {
            return None;
        }
        let pf = if comps == 1 { GUID_WICPixelFormat8bppGray } else { GUID_WICPixelFormat24bppRGB };
        f.CreateBitmapFromMemory(w, h, &pf, stride, &raw[..stride as usize * h as usize]).ok()?.cast().ok()?
    };

    let px = conv.read_pixels(&source, target).ok()?;
    let px = conv.resize(px, MAX_SIDE).ok()?;
    let data = conv.encode_jpeg_mem(&px, QUALITY).ok()?;
    ((data.len() as f64) < s.content.len() as f64 * MIN_GAIN).then_some(Pixels { data, ..px })
}
