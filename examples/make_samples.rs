//! Génère des fichiers de test pour les conversions (images à piège + PDF lourd).
//!   cargo run --release --example make_samples -- <dossier> <grosse_photo.jpg>
//!
//! - alpha.png      : PNG transparent (doit sortir sur fond blanc en JPG)
//! - rot6.jpg       : JPEG 400x200 marqué orientation 6 (doit sortir en 200x400, rouge en haut)
//! - photo.bmp/.tif : grosses images sans compression
//! - lourd.pdf      : 2 pages, une photo JPEG pleine résolution + une image RGB en Flate

use std::path::{Path, PathBuf};

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use windows::core::{w, Interface, HSTRING, PCWSTR, PROPVARIANT};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(&args[0]);
    let photo = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;

        // alpha.png : moitié gauche bleue opaque, moitié droite transparente.
        let (w, h) = (800u32, 600u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                if x < w / 2 {
                    rgba[i..i + 4].copy_from_slice(&[30, 90, 220, 255]);
                }
            }
        }
        let mut enc = png::Encoder::new(std::fs::File::create(dir.join("alpha.png")).unwrap(), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();

        // rot6.jpg : moitié gauche rouge ; orientation 6 = rotation de 90° horaire à l'affichage.
        let (w, h) = (400u32, 200u32);
        let mut bgr = vec![255u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w / 2 {
                let i = ((y * w + x) * 3) as usize;
                bgr[i..i + 3].copy_from_slice(&[0, 0, 255]);
            }
        }
        let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat24bppBGR, w * 3, &bgr)?;
        encode(&f, &bmp.cast()?, &dir.join("rot6.jpg"), &GUID_ContainerFormatJpeg, Some(6))?;

        // Grosse photo en BMP / TIFF.
        let p = HSTRING::from(photo.as_os_str());
        let dec = f.CreateDecoderFromFilename(PCWSTR(p.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
        let frame: IWICBitmapSource = dec.GetFrame(0)?.cast()?;
        encode(&f, &frame, &dir.join("photo.bmp"), &GUID_ContainerFormatBmp, None)?;
        encode(&f, &frame, &dir.join("photo.tif"), &GUID_ContainerFormatTiff, None)?;
    }

    make_pdf(&dir.join("lourd.pdf"), &std::fs::read(&photo).unwrap(), jpeg_size(&photo));
    println!("ok : {}", dir.display());
    Ok(())
}

unsafe fn encode(
    f: &IWICImagingFactory,
    src: &IWICBitmapSource,
    out: &Path,
    container: &windows::core::GUID,
    orientation: Option<u16>,
) -> windows::core::Result<()> {
    let o = HSTRING::from(out.as_os_str());
    let stream = f.CreateStream()?;
    stream.InitializeFromFilename(PCWSTR(o.as_ptr()), GENERIC_WRITE.0)?;
    let enc = f.CreateEncoder(container, std::ptr::null())?;
    enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
    let mut fe = None;
    let mut bag = None;
    enc.CreateNewFrame(&mut fe, &mut bag)?;
    let fe: IWICBitmapFrameEncode = fe.unwrap();
    fe.Initialize(bag.as_ref())?;
    if let Some(o) = orientation {
        fe.GetMetadataQueryWriter()?.SetMetadataByName(w!("/app1/ifd/{ushort=274}"), &PROPVARIANT::from(o))?;
    }
    fe.WriteSource(src, std::ptr::null())?;
    fe.Commit()?;
    enc.Commit()
}

fn jpeg_size(p: &Path) -> (u32, u32) {
    unsafe {
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
        let s = HSTRING::from(p.as_os_str());
        let d = f.CreateDecoderFromFilename(PCWSTR(s.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).unwrap();
        let (mut w, mut h) = (0, 0);
        d.GetFrame(0).unwrap().GetSize(&mut w, &mut h).unwrap();
        (w, h)
    }
}

fn make_pdf(out: &Path, jpeg: &[u8], (jw, jh): (u32, u32)) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });

    // Page 1 : photo JPEG pleine résolution.
    let img1 = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => jw as i64, "Height" => jh as i64,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode",
        },
        jpeg.to_vec(),
    ));
    // Page 2 : image RGB 3000x2000 (dégradé + bruit) en Flate.
    let (w, h) = (3000u32, 2000u32);
    let mut raw = Vec::with_capacity((w * h * 3) as usize);
    let mut seed = 1u32;
    for y in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let n = (seed >> 27) as u8;
            raw.extend_from_slice(&[(x * 255 / w) as u8 ^ n, (y * 255 / h) as u8, 128u8.wrapping_add(n)]);
        }
    }
    let mut s2 = Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
        },
        raw,
    );
    s2.compress().unwrap();
    let img2 = doc.add_object(s2);

    let mut kids = Vec::new();
    for (img, title) in [(img1, "Photo"), (img2, "Degrade")] {
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new("cm", vec![500.into(), 0.into(), 0.into(), 375.into(), 47.into(), 300.into()]),
                Operation::new("Do", vec!["Im".into()]),
                Operation::new("Q", vec![]),
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![60.into(), 750.into()]),
                Operation::new("Tj", vec![Object::string_literal(title)]),
                Operation::new("ET", vec![]),
            ],
        };
        let cid = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let pid = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => cid,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im" => img },
                "Font" => dictionary! { "F1" => font_id },
            },
        });
        kids.push(pid.into());
    }
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Count" => 2, "Kids" => kids }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.save(out).unwrap();
}
