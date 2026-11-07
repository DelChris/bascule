//! Conversion. Décodage : WIC (codecs de Windows + extensions HEIF / HEVC / AV1 / WebP).
//! Encodage : JPG via WIC, PNG via le crate `png` (fdeflate), ~10x plus rapide que l'encodeur PNG de WIC.
//! PDF : voir `pdf.rs` (réutilise l'encodeur JPG d'ici).

use std::borrow::Cow;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::time::Instant;

use windows::core::{w, Interface, GUID, HSTRING, PCWSTR, PROPVARIANT, PWSTR, VARIANT};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IStream, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STREAM_SEEK_END,
    STREAM_SEEK_SET,
};
use windows::Win32::UI::Shell::SHCreateMemStream;

use crate::formats::{Format, REDUCE_MAX_SIDE, REDUCE_QUALITY};

/// Petite image HEIC (64x64) décodée au démarrage pour initialiser Media Foundation / HEVC
/// (~0,9 s la première fois) avant que l'utilisateur ne demande quoi que ce soit.
static WARMUP_HEIC: &[u8] = include_bytes!("../assets/warmup.heic");

pub(crate) enum Error {
    Win(windows::core::Error),
    Msg(String),
}

impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Self::Win(e)
    }
}
impl From<png::EncodingError> for Error {
    fn from(e: png::EncodingError) -> Self {
        Self::Msg(format!("encodage PNG : {e}"))
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Msg(format!("écriture : {e}"))
    }
}
impl From<String> for Error {
    fn from(e: String) -> Self {
        Self::Msg(e)
    }
}

pub(crate) fn trace(what: &str, t: Instant) {
    if std::env::var_os("BASCULE_TRACE").is_some() {
        eprintln!("{what} {:?}", t.elapsed());
    }
}

/// Image décodée en mémoire, prête à encoder.
pub(crate) struct Pixels {
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub format: GUID,
    pub data: Vec<u8>,
}

pub struct Converter {
    pub(crate) factory: IWICImagingFactory,
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
        let out = reserve_output(src, fmt.out_ext(), fmt.suffix())
            .map_err(|e| format!("création du fichier impossible : {e}"))?;
        let r = match fmt {
            Format::Compress => crate::pdf::compress(self, src, &out),
            _ => unsafe { self.convert_image(src, &out, fmt, jpeg_quality) },
        };
        match r {
            Ok(()) => Ok(out),
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                Err(match e {
                    Error::Win(e) => describe(&e, src),
                    Error::Msg(m) => m,
                })
            }
        }
    }

    unsafe fn convert_image(&self, src: &Path, out: &Path, fmt: Format, jpeg_quality: u32) -> Result<(), Error> {
        let f = &self.factory;
        let src_w = HSTRING::from(src.as_os_str());
        let decoder = f.CreateDecoderFromFilename(PCWSTR(src_w.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let alpha = self.has_alpha(&frame.GetPixelFormat()?);
        let icc = self.icc_contexts(&frame).is_some();

        // Orientation : le décodeur HEIF applique lui-même irot/imir (vérifié : un HEIC
        // 64x32 marqué « orientation 6 » ressort en 32x64), mais pas les décodeurs JPEG / TIFF.
        let mut source: IWICBitmapSource = frame.cast()?;
        let transform = exif_transform(&decoder, &frame);
        if transform != WICBitmapTransformRotate0 {
            let fr = f.CreateBitmapFlipRotator()?;
            fr.Initialize(&source, transform)?;
            source = fr.cast()?;
        }

        // Ordre des canaux selon l'encodeur : RGB pour `png`, BGR (natif) pour WIC.
        // Vers JPG, une source transparente est décodée prémultipliée puis posée sur du blanc.
        let target = match (fmt, alpha) {
            (Format::Png, true) => GUID_WICPixelFormat32bppRGBA,
            (Format::Png, false) => GUID_WICPixelFormat24bppRGB,
            (_, true) => GUID_WICPixelFormat32bppPBGRA,
            (_, false) => GUID_WICPixelFormat24bppBGR,
        };

        // Décodage plein cadre en un seul appel : un consommateur qui lit par bandes
        // (encodeur, redimensionneur) ferait redécoder l'image HEIF à chaque bande.
        let t = Instant::now();
        let mut px = self.read_pixels(&source, target)?;
        trace("decode", t);

        if fmt == Format::Reduce && px.w.max(px.h) > REDUCE_MAX_SIDE {
            let t = Instant::now();
            px = self.resize(px, REDUCE_MAX_SIDE)?;
            trace("resize", t);
        }
        if target == GUID_WICPixelFormat32bppPBGRA {
            px = flatten_on_white(px);
        }

        let t = Instant::now();
        match fmt {
            Format::Png => write_png(out, &px, alpha, if icc { self.icc_profile(&frame) } else { None })?,
            _ => {
                let q = if fmt == Format::Reduce { REDUCE_QUALITY } else { jpeg_quality };
                let out_w = HSTRING::from(out.as_os_str());
                let stream = f.CreateStream()?;
                stream.InitializeFromFilename(PCWSTR(out_w.as_ptr()), GENERIC_WRITE.0)?;
                self.encode_jpeg(&stream.cast()?, &px, q, if icc { Some(&frame) } else { None })?;
            }
        }
        trace("encode", t);
        Ok(())
    }

    /// Convertit `source` au format `target` et copie toute l'image en mémoire.
    pub(crate) unsafe fn read_pixels(&self, source: &IWICBitmapSource, target: GUID) -> windows::core::Result<Pixels> {
        let conv = self.factory.CreateFormatConverter()?;
        conv.Initialize(source, &target, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h)?;
        let stride = w * bytes_per_pixel(&target);
        let mut data = vec![0u8; stride as usize * h as usize];
        conv.CopyPixels(std::ptr::null(), stride, &mut data)?;
        Ok(Pixels { w, h, stride, format: target, data })
    }

    /// Réduit l'image pour que son plus grand côté fasse `max_side` (bicubique haute qualité).
    pub(crate) unsafe fn resize(&self, px: Pixels, max_side: u32) -> windows::core::Result<Pixels> {
        let long = px.w.max(px.h);
        if long <= max_side {
            return Ok(px);
        }
        let scale = |v: u32| ((v as u64 * max_side as u64 + long as u64 / 2) / long as u64).max(1) as u32;
        let (nw, nh) = (scale(px.w), scale(px.h));
        let bmp = self.factory.CreateBitmapFromMemory(px.w, px.h, &px.format, px.stride, &px.data)?;
        let scaler = self.factory.CreateBitmapScaler()?;
        scaler.Initialize(&bmp, nw, nh, WICBitmapInterpolationModeHighQualityCubic)?;
        let stride = nw * bytes_per_pixel(&px.format);
        let mut data = vec![0u8; stride as usize * nh as usize];
        scaler.CopyPixels(std::ptr::null(), stride, &mut data)?;
        Ok(Pixels { w: nw, h: nh, stride, format: px.format, data })
    }

    /// Encode en JPG dans `stream`. `px` : 24bppBGR ou 8bppGray.
    pub(crate) unsafe fn encode_jpeg(
        &self,
        stream: &IStream,
        px: &Pixels,
        quality: u32,
        icc_from: Option<&IWICBitmapFrameDecode>,
    ) -> windows::core::Result<()> {
        let encoder = self.factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
        encoder.Initialize(stream, WICBitmapEncoderNoCache)?;

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
        fe.SetSize(px.w, px.h)?;
        let mut pf = px.format;
        fe.SetPixelFormat(&mut pf)?;
        if let Some(ctx) = icc_from.and_then(|fr| self.icc_contexts(fr)) {
            let _ = fe.SetColorContexts(&ctx);
        }
        fe.WritePixels(px.h, px.stride, &px.data)?;
        fe.Commit()?;
        encoder.Commit()
    }

    /// Encode en JPG en mémoire.
    pub(crate) unsafe fn encode_jpeg_mem(&self, px: &Pixels, quality: u32) -> windows::core::Result<Vec<u8>> {
        let stream = SHCreateMemStream(None).ok_or_else(windows::core::Error::empty)?;
        self.encode_jpeg(&stream, px, quality, None)?;
        let mut size = 0u64;
        stream.Seek(0, STREAM_SEEK_END, Some(&mut size))?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut buf = vec![0u8; size as usize];
        let mut read = 0u32;
        stream.Read(buf.as_mut_ptr() as *mut c_void, buf.len() as u32, Some(&mut read)).ok()?;
        buf.truncate(read as usize);
        Ok(buf)
    }

    pub(crate) unsafe fn has_alpha(&self, pf: &GUID) -> bool {
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

pub(crate) fn bytes_per_pixel(pf: &GUID) -> u32 {
    if *pf == GUID_WICPixelFormat8bppGray {
        1
    } else if *pf == GUID_WICPixelFormat24bppBGR || *pf == GUID_WICPixelFormat24bppRGB {
        3
    } else {
        4
    }
}

/// Orientation EXIF des JPEG / TIFF -> transformation WIC.
unsafe fn exif_transform(decoder: &IWICBitmapDecoder, frame: &IWICBitmapFrameDecode) -> WICBitmapTransformOptions {
    let query = match decoder.GetContainerFormat() {
        Ok(c) if c == GUID_ContainerFormatJpeg => w!("/app1/ifd/{ushort=274}"),
        Ok(c) if c == GUID_ContainerFormatTiff => w!("/ifd/{ushort=274}"),
        _ => return WICBitmapTransformRotate0,
    };
    let orientation = frame
        .GetMetadataQueryReader()
        .and_then(|r| {
            let mut v = PROPVARIANT::default();
            r.GetMetadataByName(query, &mut v)?;
            u16::try_from(&v)
        })
        .unwrap_or(1);
    let (rot, flip) = match orientation {
        2 => (WICBitmapTransformRotate0, true),
        3 => (WICBitmapTransformRotate180, false),
        4 => (WICBitmapTransformRotate180, true),
        5 => (WICBitmapTransformRotate90, true),
        6 => (WICBitmapTransformRotate90, false),
        7 => (WICBitmapTransformRotate270, true),
        8 => (WICBitmapTransformRotate270, false),
        _ => (WICBitmapTransformRotate0, false),
    };
    if flip { WICBitmapTransformOptions(rot.0 | WICBitmapTransformFlipHorizontal.0) } else { rot }
}

/// 32bppPBGRA (prémultiplié) -> 24bppBGR sur fond blanc.
fn flatten_on_white(px: Pixels) -> Pixels {
    let stride = px.w * 3;
    let mut data = vec![0u8; stride as usize * px.h as usize];
    for y in 0..px.h as usize {
        let src = &px.data[y * px.stride as usize..][..px.w as usize * 4];
        let dst = &mut data[y * stride as usize..][..px.w as usize * 3];
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(3)) {
            let bg = 255 - s[3];
            d[0] = s[0].saturating_add(bg);
            d[1] = s[1].saturating_add(bg);
            d[2] = s[2].saturating_add(bg);
        }
    }
    Pixels { w: px.w, h: px.h, stride, format: GUID_WICPixelFormat24bppBGR, data }
}

fn write_png(out: &Path, px: &Pixels, alpha: bool, icc: Option<Vec<u8>>) -> Result<(), Error> {
    let file = BufWriter::with_capacity(1 << 20, File::create(out)?);
    let mut info = png::Info::with_size(px.w, px.h);
    info.color_type = if alpha { png::ColorType::Rgba } else { png::ColorType::Rgb };
    info.bit_depth = png::BitDepth::Eight;
    info.icc_profile = icc.map(Cow::Owned);
    let mut enc = png::Encoder::with_info(file, info)?;
    // Mesuré sur une photo 18 Mpx : Fast+Paeth = 84 ms / 24,5 Mo,
    // Default+Paeth = 4,5 s / 22,6 Mo, encodeur PNG de WIC = 1,3 s / 22,9 Mo.
    enc.set_compression(png::Compression::Fast);
    enc.set_filter(png::FilterType::Paeth);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&px.data)?;
    writer.finish()?;
    Ok(())
}

/// `photo{suffixe}.png`, ou `photo{suffixe} (1).png` s'il existe déjà. La création
/// atomique évite que deux conversions parallèles choisissent le même nom.
fn reserve_output(src: &Path, ext: &str, suffix: &str) -> io::Result<PathBuf> {
    let dir = src.parent().unwrap_or(Path::new("."));
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    for i in 0..10_000 {
        let name = if i == 0 { format!("{stem}{suffix}.{ext}") } else { format!("{stem}{suffix} ({i}).{ext}") };
        let p = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "trop de fichiers homonymes"))
}

/// Message d'erreur en français ; pour un codec manquant, nomme l'extension du Store à installer.
fn describe(e: &windows::core::Error, src: &Path) -> String {
    let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match e.code().0 as u32 {
        0x88982F50 | 0x88982F07 => match ext.as_str() {
            "heic" | "heif" | "hif" => "format non reconnu : installez « Extensions d'images HEIF » (Microsoft Store)".into(),
            "avif" => "format non reconnu : installez « Extension vidéo AV1 » (Microsoft Store)".into(),
            "webp" => "format non reconnu : installez « Extensions d'images web » (Microsoft Store)".into(),
            _ => "format non reconnu ou fichier endommagé".into(),
        },
        0xC00D5212 | 0xC00D36B5 => match ext.as_str() {
            "avif" => "décodeur AV1 absent : installez « Extension vidéo AV1 » (Microsoft Store)".into(),
            _ => "décodeur HEVC absent : installez « Extensions vidéo HEVC » (Microsoft Store)".into(),
        },
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
