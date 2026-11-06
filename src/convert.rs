//! Conversion. Décodage : WIC (extensions HEIF + HEVC de Windows).
//! Encodage : JPG via WIC, PNG via le crate `png` (fdeflate), ~10x plus rapide que l'encodeur PNG de WIC.

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Instant;

use windows::core::{Interface, GUID, HSTRING, PCWSTR, PWSTR, VARIANT};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

use crate::formats::Format;

/// Petite image HEIC (64x64) décodée au démarrage pour initialiser Media Foundation / HEVC
/// (~0,9 s la première fois) avant que l'utilisateur ne demande quoi que ce soit.
static WARMUP_HEIC: &[u8] = include_bytes!("../assets/warmup.heic");

struct Error(String);

impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Self(describe(&e))
    }
}
impl From<png::EncodingError> for Error {
    fn from(e: png::EncodingError) -> Self {
        Self(format!("encodage PNG : {e}"))
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self(format!("écriture : {e}"))
    }
}

fn trace(what: &str, t: Instant) {
    if std::env::var_os("CONVERTISSEUR_TRACE").is_some() {
        eprintln!("{what} {:?}", t.elapsed());
    }
}

pub struct Converter {
    factory: IWICImagingFactory,
}

impl Converter {
    /// À créer une fois par thread (initialise COM en MTA sur ce thread).
    pub fn new() -> Result<Self, String> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(|e| e.message())?;
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|e| e.message())?;
            Ok(Self { factory })
        }
    }

    pub fn warm_up(&self) {
        unsafe {
            let r = (|| -> windows::core::Result<()> {
                let stream = self.factory.CreateStream()?;
                stream.InitializeFromMemory(WARMUP_HEIC)?;
                let dec = self.factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
                let frame = dec.GetFrame(0)?;
                let mut buf = vec![0u8; 64 * 64 * 4];
                frame.CopyPixels(std::ptr::null(), 64 * 4, &mut buf)
            })();
            let _ = r;
        }
    }

    /// Convertit `src` dans le même dossier. Retourne le chemin créé.
    pub fn convert(&self, src: &Path, fmt: Format, jpeg_quality: u32) -> Result<PathBuf, String> {
        let out = reserve_output(src, fmt.ext()).map_err(|e| format!("création du fichier impossible : {e}"))?;
        match unsafe { self.run(src, &out, fmt, jpeg_quality) } {
            Ok(()) => Ok(out),
            Err(Error(e)) => {
                let _ = std::fs::remove_file(&out);
                Err(e)
            }
        }
    }

    unsafe fn run(&self, src: &Path, out: &Path, fmt: Format, jpeg_quality: u32) -> Result<(), Error> {
        let f = &self.factory;
        let src_w = HSTRING::from(src.as_os_str());
        let decoder = f.CreateDecoderFromFilename(
            PCWSTR(src_w.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        // Orientation : le décodeur HEIF de WIC applique lui-même irot/imir (vérifié :
        // un HEIC 64x32 marqué « orientation 6 » ressort en 32x64, orientation 1).
        let alpha = self.has_alpha(&frame.GetPixelFormat()?);
        let icc = self.icc_profile(&frame);

        // Ordre des canaux selon l'encodeur : RGB pour `png`, BGR (natif) pour WIC.
        let (target, bpp) = match (fmt, alpha) {
            (Format::Png, true) => (GUID_WICPixelFormat32bppRGBA, 4),
            (Format::Png, false) => (GUID_WICPixelFormat24bppRGB, 3),
            (Format::Jpg, _) => (GUID_WICPixelFormat24bppBGR, 3),
        };
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(&frame, &target, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;

        // Décodage plein cadre en un seul appel : l'encodeur WIC lit sinon la source par
        // bandes, et le décodeur HEIF redécoderait l'image à chaque bande.
        let t = Instant::now();
        let stride = w as usize * bpp;
        let mut pixels = vec![0u8; stride * h as usize];
        conv.CopyPixels(std::ptr::null(), stride as u32, &mut pixels)?;
        trace("decode", t);

        let t = Instant::now();
        match fmt {
            Format::Png => write_png(out, w, h, alpha, &pixels, icc)?,
            Format::Jpg => self.write_jpeg(out, w, h, stride as u32, &mut pixels, target, icc.is_some().then_some(&frame), jpeg_quality)?,
        }
        trace("encode", t);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn write_jpeg(
        &self,
        out: &Path,
        w: u32,
        h: u32,
        stride: u32,
        pixels: &mut [u8],
        pf: GUID,
        icc_from: Option<&IWICBitmapFrameDecode>,
        quality: u32,
    ) -> windows::core::Result<()> {
        let f = &self.factory;
        let out_w = HSTRING::from(out.as_os_str());
        let stream = f.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(out_w.as_ptr()), GENERIC_WRITE.0)?;
        let encoder = f.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;

        let mut fe: Option<IWICBitmapFrameEncode> = None;
        let mut bag: Option<IPropertyBag2> = None;
        encoder.CreateNewFrame(&mut fe, &mut bag)?;
        let fe = fe.ok_or_else(windows::core::Error::empty)?;
        if let Some(bag) = bag.as_ref() {
            let mut name: Vec<u16> = "ImageQuality\0".encode_utf16().collect();
            let prop = PROPBAG2 { pstrName: PWSTR(name.as_mut_ptr()), ..Default::default() };
            bag.Write(1, &prop, &VARIANT::from(quality.clamp(1, 100) as f32 / 100.0))?;
        }
        fe.Initialize(bag.as_ref())?;
        fe.SetSize(w, h)?;
        let mut pf = pf;
        fe.SetPixelFormat(&mut pf)?;
        if let Some(ctx) = icc_from.and_then(|fr| self.icc_contexts(fr)) {
            let _ = fe.SetColorContexts(&ctx);
        }
        fe.WritePixels(h, stride, pixels)?;
        fe.Commit()?;
        encoder.Commit()
    }

    unsafe fn has_alpha(&self, pf: &GUID) -> bool {
        self.factory
            .CreateComponentInfo(pf)
            .and_then(|i| i.cast::<IWICPixelFormatInfo2>())
            .and_then(|i| i.SupportsTransparency())
            .map(|b| b.as_bool())
            .unwrap_or(false)
    }

    /// Contextes couleur de type « profil ICC » (ex. Display P3 des iPhone).
    unsafe fn icc_contexts(&self, frame: &IWICBitmapFrameDecode) -> Option<Vec<Option<IWICColorContext>>> {
        let mut count = 0u32;
        frame.GetColorContexts(&mut [], &mut count).ok()?;
        if count == 0 {
            return None;
        }
        let mut ctx: Vec<Option<IWICColorContext>> =
            (0..count).map(|_| self.factory.CreateColorContext().ok()).collect();
        if ctx.iter().any(Option::is_none) {
            return None;
        }
        frame.GetColorContexts(&mut ctx, &mut count).ok()?;
        let icc: Vec<_> = ctx
            .into_iter()
            .filter(|c| c.as_ref().and_then(|c| c.GetType().ok()) == Some(WICColorContextProfile))
            .take(1)
            .collect();
        (!icc.is_empty()).then_some(icc)
    }

    unsafe fn icc_profile(&self, frame: &IWICBitmapFrameDecode) -> Option<Vec<u8>> {
        let ctx = self.icc_contexts(frame)?.into_iter().next()??;
        let mut size = 0u32;
        ctx.GetProfileBytes(&mut [], &mut size).ok()?;
        let mut bytes = vec![0u8; size as usize];
        ctx.GetProfileBytes(&mut bytes, &mut size).ok()?;
        Some(bytes)
    }
}

fn write_png(out: &Path, w: u32, h: u32, alpha: bool, pixels: &[u8], icc: Option<Vec<u8>>) -> Result<(), Error> {
    let file = BufWriter::with_capacity(1 << 20, File::create(out)?);
    let mut info = png::Info::with_size(w, h);
    info.color_type = if alpha { png::ColorType::Rgba } else { png::ColorType::Rgb };
    info.bit_depth = png::BitDepth::Eight;
    info.icc_profile = icc.map(Cow::Owned);
    let mut enc = png::Encoder::with_info(file, info)?;
    // Mesuré sur une photo 18 Mpx : Fast+Paeth = 84 ms / 24,5 Mo,
    // Default+Paeth = 4,5 s / 22,6 Mo, encodeur PNG de WIC = 1,3 s / 22,9 Mo.
    enc.set_compression(png::Compression::Fast);
    enc.set_filter(png::FilterType::Paeth);
    let mut writer = enc.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(())
}

/// `photo.png`, ou `photo (1).png` s'il existe déjà. La création atomique évite
/// que deux conversions parallèles choisissent le même nom.
fn reserve_output(src: &Path, ext: &str) -> io::Result<PathBuf> {
    let dir = src.parent().unwrap_or(Path::new("."));
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    for i in 0..10_000 {
        let name = if i == 0 { format!("{stem}.{ext}") } else { format!("{stem} ({i}).{ext}") };
        let p = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "trop de fichiers homonymes"))
}

fn describe(e: &windows::core::Error) -> String {
    match e.code().0 as u32 {
        0x88982F50 | 0x88982F07 => {
            "format non reconnu : installez « Extensions d'images HEIF » (Microsoft Store)".into()
        }
        0xC00D5212 | 0xC00D36B5 => {
            "décodeur HEVC absent : installez « Extensions vidéo HEVC » (Microsoft Store)".into()
        }
        0x80070002 | 0x80070003 => "fichier introuvable".into(),
        0x80070005 => "accès refusé".into(),
        0x80070020 => "fichier utilisé par un autre programme".into(),
        c => {
            let m = e.message();
            let m = m.trim();
            if m.is_empty() { format!("erreur 0x{c:08X}") } else { format!("{m} (0x{c:08X})") }
        }
    }
}
