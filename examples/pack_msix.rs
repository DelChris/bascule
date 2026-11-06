//! Empaqueteur MSIX minimal (remplace `makeappx pack`) via l'API Windows IAppxFactory.
//!   cargo run --release --example pack_msix -- <dossier> <sortie.msix>
//! Le dossier doit contenir AppxManifest.xml ; tous les autres fichiers deviennent la charge utile.

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{FALSE, TRUE};
use windows::Win32::Storage::Packaging::Appx::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::SHCreateStreamOnFileEx;

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("ico") => "image/vnd.microsoft.icon",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

fn read_stream(p: &Path) -> windows::core::Result<IStream> {
    unsafe { SHCreateStreamOnFileEx(&HSTRING::from(p.as_os_str()), (STGM_READ | STGM_SHARE_DENY_WRITE).0, 0, FALSE, None) }
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let factory: IAppxFactory = CoCreateInstance(&AppxFactory, None, CLSCTX_INPROC_SERVER)?;
        let _ = std::fs::remove_file(&out);
        let stream = SHCreateStreamOnFileEx(
            &HSTRING::from(out.as_os_str()),
            (STGM_CREATE | STGM_WRITE | STGM_SHARE_EXCLUSIVE).0,
            0,
            TRUE,
            None,
        )?;
        let sha256 = CreateUri(w!("http://www.w3.org/2001/04/xmlenc#sha256"), URI_CREATE_FLAGS(0), 0)?;
        let settings = APPX_PACKAGE_SETTINGS { forceZip32: TRUE, hashMethod: ManuallyDrop::new(Some(sha256)) };
        let writer = factory.CreatePackageWriter(&stream, &settings)?;

        let mut all = Vec::new();
        files(&dir, &mut all);
        for f in &all {
            let rel = f.strip_prefix(&dir).unwrap();
            if rel.as_os_str().eq_ignore_ascii_case("AppxManifest.xml") {
                continue;
            }
            let name = HSTRING::from(rel.to_string_lossy().replace('/', "\\"));
            writer.AddPayloadFile(&name, &HSTRING::from(content_type(f)), APPX_COMPRESSION_OPTION_NORMAL, &read_stream(f)?)?;
            println!("  + {}", rel.display());
        }
        writer.Close(&read_stream(&dir.join("AppxManifest.xml"))?)?;
        println!("{} : {} octets", out.display(), std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0));
    }
    Ok(())
}
