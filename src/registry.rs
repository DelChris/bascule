//! Menu contextuel d'Explorer, démarrage auto et réglages (tout en HKCU, pas besoin d'admin).

use std::path::Path;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

use crate::formats::SOURCES;
use crate::util::wide;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_KEY: &str = r"Software\Bascule";
const VALUE_NAME: &str = "Bascule";

fn menu_key(ext: &str) -> String {
    format!(r"Software\Classes\SystemFileAssociations\{ext}\shell\Bascule")
}

fn check(e: WIN32_ERROR) -> windows::core::Result<()> {
    e.ok()
}

fn set_value(path: &str, name: Option<&str>, data: &[u8], kind: REG_VALUE_TYPE) -> windows::core::Result<()> {
    unsafe {
        let mut key = HKEY::default();
        check(RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        ))?;
        let name = name.map(HSTRING::from);
        let pname = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
        let r = RegSetValueExW(key, pname, 0, kind, Some(data));
        let _ = RegCloseKey(key);
        check(r)
    }
}

fn set_str(path: &str, name: Option<&str>, value: &str) -> windows::core::Result<()> {
    let w = wide(value);
    let bytes = unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2) };
    set_value(path, name, bytes, REG_SZ)
}

fn get_str(path: &str, name: &str) -> Option<String> {
    unsafe {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        r.is_ok().then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
    }
}

fn delete_tree(path: &str) {
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(path));
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(path));
    }
}

/// Crée « Convertir... » > PNG / JPG sur chaque extension source.
pub fn install_context_menu(exe: &Path, icon: &Path) -> windows::core::Result<()> {
    let exe = exe.display();
    for (ext, targets) in SOURCES {
        let base = menu_key(ext);
        delete_tree(&base);
        set_str(&base, Some("MUIVerb"), "Convertir...")?;
        set_str(&base, Some("Icon"), &icon.display().to_string())?;
        set_str(&base, Some("SubCommands"), "")?;
        // "Player" : le menu reste visible même au-delà de 15 fichiers sélectionnés.
        set_str(&base, Some("MultiSelectModel"), "Player")?;
        for (i, f) in targets.iter().enumerate() {
            let sub = format!(r"{base}\shell\{:02}{}", i + 1, f.ext());
            set_str(&sub, Some("MUIVerb"), f.label())?;
            set_str(&sub, Some("MultiSelectModel"), "Player")?;
            set_str(&format!(r"{sub}\command"), None, &format!("\"{exe}\" --to {} \"%1\"", f.ext()))?;
        }
    }
    notify_shell();
    Ok(())
}

pub fn uninstall_context_menu() {
    for (ext, _) in SOURCES {
        delete_tree(&menu_key(ext));
    }
    notify_shell();
}

pub fn context_menu_installed() -> bool {
    get_str(&menu_key(SOURCES[0].0), "MUIVerb").is_some()
}

pub fn set_autostart(exe: Option<&Path>) -> windows::core::Result<()> {
    match exe {
        Some(exe) => set_str(RUN_KEY, Some(VALUE_NAME), &format!("\"{}\" --tray", exe.display())),
        None => unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), &HSTRING::from(VALUE_NAME));
            Ok(())
        },
    }
}

pub fn autostart_enabled() -> bool {
    get_str(RUN_KEY, VALUE_NAME).is_some()
}

pub fn jpeg_quality() -> u32 {
    unsafe {
        let mut v = 0u32;
        let mut size = 4u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(APP_KEY),
            &HSTRING::from("JpegQuality"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        );
        if r.is_ok() && (1..=100).contains(&v) { v } else { 90 }
    }
}

pub fn set_jpeg_quality(q: u32) {
    let _ = set_value(APP_KEY, Some("JpegQuality"), &q.to_le_bytes(), REG_DWORD);
}

/// Mémorise que le menu Windows 11 (package sparse) est installé.
pub fn set_shell_package(on: bool) {
    if on {
        let _ = set_value(APP_KEY, Some("ShellPackage"), &1u32.to_le_bytes(), REG_DWORD);
    } else {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(APP_KEY), &HSTRING::from("ShellPackage"));
        }
    }
}

pub fn shell_package_installed() -> bool {
    unsafe {
        let mut v = 0u32;
        let mut size = 4u32;
        RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(APP_KEY),
            &HSTRING::from("ShellPackage"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
        .is_ok()
            && v == 1
    }
}

/// Windows 11 = build 22000 et plus.
pub fn is_windows_11() -> bool {
    unsafe {
        let mut buf = [0u16; 32];
        let mut size = (buf.len() * 2) as u32;
        let r = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            &HSTRING::from("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        let s = String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]);
        r.is_ok() && s.trim().parse::<u32>().is_ok_and(|b| b >= 22000)
    }
}

pub fn remove_settings() {
    delete_tree(APP_KEY);
}

fn notify_shell() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}
