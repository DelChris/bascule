//! Génère assets/warmup.heic (petite image HEIC embarquée dans l'exe pour
//! préchauffer le décodeur HEVC au démarrage du tray).
//!   cargo run --example make_warmup

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};

fn main() -> windows::core::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        std::fs::create_dir_all("assets").unwrap();
        let path = HSTRING::from("assets/warmup.heic");
        let stream = f.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(path.as_ptr()), GENERIC_WRITE.0)?;
        let enc = f.CreateEncoder(&GUID_ContainerFormatHeif, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut fe = None;
        enc.CreateNewFrame(&mut fe, std::ptr::null_mut())?;
        let fe: IWICBitmapFrameEncode = fe.unwrap();
        fe.Initialize(None)?;
        let (w, h) = (64u32, 64u32);
        fe.SetSize(w, h)?;
        let mut pf = GUID_WICPixelFormat32bppBGR;
        fe.SetPixelFormat(&mut pf)?;
        let mut px = vec![0u8; (w * h * 4) as usize];
        for (i, p) in px.chunks_mut(4).enumerate() {
            p[0] = (i % 64 * 4) as u8;
            p[1] = (i / 64 * 4) as u8;
            p[2] = 128;
        }
        fe.WritePixels(h, w * 4, &px)?;
        fe.Commit()?;
        enc.Commit()?;
        println!("assets/warmup.heic : {} octets", std::fs::metadata("assets/warmup.heic").unwrap().len());
    }
    Ok(())
}
