//! Teste la DLL d'extension sans l'enregistrer : charge ses exports COM comme le
//! ferait l'Explorateur, affiche le menu obtenu et invoque un sous-menu.
//!   cargo run --release -p bascule_shell --example test_dll -- <png|jpg|-> <fichiers...>

use std::ffi::c_void;

use windows::core::{s, Interface, GUID, HRESULT, HSTRING};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, IClassFactory, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;

const CLSID: GUID = GUID::from_u128(0x7DB95F0E_6E72_44FB_8DAD_A425EB2D3CA0);
type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;

fn take(p: windows::core::PWSTR) -> String {
    let s = unsafe { p.to_string().unwrap_or_default() };
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fmt = args[0].clone();
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let dll = LoadLibraryW(&HSTRING::from("target/release/bascule_shell.dll"))?;
        let get: GetClassObject = std::mem::transmute(GetProcAddress(dll, s!("DllGetClassObject")).unwrap());
        let mut p = std::ptr::null_mut();
        get(&CLSID, &IClassFactory::IID, &mut p).ok()?;
        let factory = IClassFactory::from_raw(p);
        let cmd: IExplorerCommand = factory.CreateInstance(None)?;

        let pidls: Vec<*mut ITEMIDLIST> = args[1..]
            .iter()
            .map(|f| {
                let mut pidl = std::ptr::null_mut();
                SHParseDisplayName(&HSTRING::from(f.as_str()), None, &mut pidl, 0, None).unwrap();
                pidl
            })
            .collect();
        let consts: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const _).collect();
        let items = SHCreateShellItemArrayFromIDLists(&consts)?;

        println!("titre  : {}", take(cmd.GetTitle(&items)?));
        println!("icône  : {}", take(cmd.GetIcon(&items)?));
        let state = cmd.GetState(&items, false)?;
        println!("état   : {} ({} élément(s))", if state == ECS_HIDDEN.0 as u32 { "masqué" } else { "visible" }, items.GetCount()?);
        println!("flags  : {:#x}", cmd.GetFlags()?);
        let subs = cmd.EnumSubCommands()?;
        loop {
            let mut one = [None];
            let mut n = 0;
            subs.Next(&mut one, Some(&mut n)).ok()?;
            if n == 0 {
                break;
            }
            let sub = one[0].take().unwrap();
            let title = take(sub.GetTitle(&items)?);
            let hidden = sub.GetState(&items, false)? == ECS_HIDDEN.0 as u32;
            println!("  sous-menu : {title}{}", if hidden { "  (masqué)" } else { "" });
            if !hidden && title.eq_ignore_ascii_case(&fmt) {
                sub.Invoke(&items, None)?;
                println!("  -> invoqué");
            }
        }
        // Laisse le thread d'envoi de la DLL finir.
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    Ok(())
}
