//! Extension shell (IExplorerCommand) : place « Convertir... » > PNG / JPG dans le
//! menu contextuel principal de Windows 11. Chargée par Explorer via le package
//! MSIX « sparse » (voir packaging/AppxManifest.xml), dans un process dllhost.
//!
//! Au clic, tous les fichiers sélectionnés partent en UN seul message vers le tray
//! (named pipe). Si le tray ne tourne pas, il est démarré.

#![allow(non_snake_case)]

#[allow(dead_code)]
#[path = "../../src/formats.rs"]
mod formats;
#[allow(dead_code)]
#[path = "../../src/ipc.rs"]
mod ipc;

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Result, GUID, HRESULT, HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::Shell::*;

use formats::{Format, SOURCES};

/// Doit correspondre à packaging/AppxManifest.xml.
pub const CLSID_CONVERT: GUID = GUID::from_u128(0x7DB95F0E_6E72_44FB_8DAD_A425EB2D3CA0);

static MODULE: AtomicIsize = AtomicIsize::new(0);
/// Objets COM vivants + envois en cours : la DLL ne doit pas être déchargée avant 0.
static LIVE: AtomicUsize = AtomicUsize::new(0);

struct Live;
impl Live {
    fn new() -> Self {
        LIVE.fetch_add(1, Ordering::SeqCst);
        Live
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

#[no_mangle]
extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(hinst.0 as isize, Ordering::SeqCst);
    }
    TRUE
}

/// Dossier de la DLL = dossier d'installation (exe, icône).
fn module_dir() -> PathBuf {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(HMODULE(MODULE.load(Ordering::SeqCst) as _), &mut buf) } as usize;
    let p = PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    p.parent().map(Path::to_path_buf).unwrap_or_default()
}

fn dup(s: &str) -> Result<PWSTR> {
    unsafe { SHStrDupW(&HSTRING::from(s)) }
}

fn supported(p: &Path) -> bool {
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase()));
    ext.is_some_and(|e| SOURCES.iter().any(|(s, _)| *s == e))
}

fn item_paths(items: Option<&IShellItemArray>) -> Vec<PathBuf> {
    let Some(items) = items else { return Vec::new() };
    unsafe {
        let n = items.GetCount().unwrap_or(0);
        (0..n)
            .filter_map(|i| {
                let item = items.GetItemAt(i).ok()?;
                let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
                let s = name.to_string().ok();
                CoTaskMemFree(Some(name.0 as *const c_void));
                s.map(PathBuf::from)
            })
            .collect()
    }
}

/// Formats proposés : union des cibles de toutes les extensions sources.
fn all_targets() -> Vec<Format> {
    let mut v: Vec<Format> = Vec::new();
    for (_, targets) in SOURCES {
        for t in *targets {
            if !v.contains(t) {
                v.push(*t);
            }
        }
    }
    v
}

fn send_or_start(msg: &str, exe: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut spawned = false;
    while ipc::send(msg, Duration::from_secs(2)).is_err() && Instant::now() < deadline {
        if !spawned {
            spawned = true;
            let _ = std::process::Command::new(exe).arg("--tray").spawn();
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

// ---------------------------------------------------------------- « Convertir... »

#[implement(IExplorerCommand)]
struct RootCommand {
    _live: Live,
}

impl IExplorerCommand_Impl for RootCommand_Impl {
    fn GetTitle(&self, _: Option<&IShellItemArray>) -> Result<PWSTR> {
        dup("Convertir...")
    }
    fn GetIcon(&self, _: Option<&IShellItemArray>) -> Result<PWSTR> {
        dup(&module_dir().join("bascule.ico").display().to_string())
    }
    fn GetToolTip(&self, _: Option<&IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(CLSID_CONVERT)
    }
    fn GetState(&self, items: Option<&IShellItemArray>, _slow: BOOL) -> Result<u32> {
        let any = item_paths(items).iter().any(|p| supported(p));
        Ok(if any { ECS_ENABLED.0 } else { ECS_HIDDEN.0 } as u32)
    }
    fn Invoke(&self, _: Option<&IShellItemArray>, _: Option<&IBindCtx>) -> Result<()> {
        Ok(())
    }
    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_HASSUBCOMMANDS.0 as u32)
    }
    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        let cmds = all_targets()
            .into_iter()
            .map(|fmt| SubCommand { fmt, _live: Live::new() }.into())
            .collect();
        Ok(CommandEnum { cmds, pos: AtomicUsize::new(0), _live: Live::new() }.into())
    }
}

// ---------------------------------------------------------------- « PNG », « JPG »

#[implement(IExplorerCommand)]
struct SubCommand {
    fmt: Format,
    _live: Live,
}

impl IExplorerCommand_Impl for SubCommand_Impl {
    fn GetTitle(&self, _: Option<&IShellItemArray>) -> Result<PWSTR> {
        dup(self.fmt.label())
    }
    fn GetIcon(&self, _: Option<&IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
    fn GetToolTip(&self, _: Option<&IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }
    fn GetCanonicalName(&self) -> Result<GUID> {
        Err(E_NOTIMPL.into())
    }
    fn GetState(&self, _: Option<&IShellItemArray>, _slow: BOOL) -> Result<u32> {
        Ok(ECS_ENABLED.0 as u32)
    }
    fn Invoke(&self, items: Option<&IShellItemArray>, _: Option<&IBindCtx>) -> Result<()> {
        let paths: Vec<String> =
            item_paths(items).into_iter().filter(|p| supported(p)).map(|p| p.display().to_string()).collect();
        if paths.is_empty() {
            return Ok(());
        }
        let msg = format!("convert {}\n{}", self.fmt.ext(), paths.join("\n"));
        let exe = module_dir().join("bascule.exe");
        // Ne pas bloquer l'Explorateur : l'envoi (et l'éventuel démarrage du tray) se fait à part.
        let live = Live::new();
        std::thread::spawn(move || {
            let _live = live;
            send_or_start(&msg, &exe);
        });
        Ok(())
    }
    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_DEFAULT.0 as u32)
    }
    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

// ---------------------------------------------------------------- énumérateur du sous-menu

#[implement(IEnumExplorerCommand)]
struct CommandEnum {
    cmds: Vec<IExplorerCommand>,
    pos: AtomicUsize,
    _live: Live,
}

impl IEnumExplorerCommand_Impl for CommandEnum_Impl {
    fn Next(&self, celt: u32, out: *mut Option<IExplorerCommand>, fetched: *mut u32) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        let mut n = 0u32;
        while n < celt {
            let i = self.pos.load(Ordering::SeqCst);
            let Some(cmd) = self.cmds.get(i) else { break };
            unsafe { out.add(n as usize).write(Some(cmd.clone())) };
            self.pos.store(i + 1, Ordering::SeqCst);
            n += 1;
        }
        if !fetched.is_null() {
            unsafe { *fetched = n };
        }
        if n == celt { S_OK } else { S_FALSE }
    }
    fn Skip(&self, celt: u32) -> Result<()> {
        let p = (self.pos.load(Ordering::SeqCst) + celt as usize).min(self.cmds.len());
        self.pos.store(p, Ordering::SeqCst);
        Ok(())
    }
    fn Reset(&self) -> Result<()> {
        self.pos.store(0, Ordering::SeqCst);
        Ok(())
    }
    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        Ok(CommandEnum {
            cmds: self.cmds.clone(),
            pos: AtomicUsize::new(self.pos.load(Ordering::SeqCst)),
            _live: Live::new(),
        }
        .into())
    }
}

// ---------------------------------------------------------------- exports COM

#[implement(IClassFactory)]
struct Factory {
    _live: Live,
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Option<&windows::core::IUnknown>, riid: *const GUID, ppv: *mut *mut c_void) -> Result<()> {
        if ppv.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let cmd: IExplorerCommand = RootCommand { _live: Live::new() }.into();
        unsafe { cmd.query(riid, ppv).ok() }
    }
    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            LIVE.fetch_add(1, Ordering::SeqCst);
        } else {
            LIVE.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[no_mangle]
unsafe extern "system" fn DllGetClassObject(rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() {
        return E_POINTER;
    }
    *ppv = std::ptr::null_mut();
    if *rclsid != CLSID_CONVERT {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory { _live: Live::new() }.into();
    factory.query(riid, ppv)
}

#[no_mangle]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE.load(Ordering::SeqCst) == 0 { S_OK } else { S_FALSE }
}
