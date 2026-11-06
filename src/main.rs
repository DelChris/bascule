#![windows_subsystem = "windows"]

//! Bascule — un seul exécutable :
//!   bascule.exe                      tray (serveur résident)
//!   bascule.exe --to png a.heic ...  envoie au tray (appelé par le menu contextuel)
//!   bascule.exe --cli png a.heic|dossier ...   conversion directe en console (tests, scripts)
//!   bascule.exe --install | --uninstall | --quit

mod convert;
mod formats;
mod icon;
mod ipc;
mod registry;
mod server;
mod tray;
mod util;

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Threading::{CreateMutexW, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK};

use convert::Converter;
use formats::{Format, SOURCES};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // Double-clic sur l'exe téléchargé (hors du dossier d'installation) : on installe.
        None if !running_from_install_dir() => install(),
        None | Some("--tray") => {
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            }
            server::run()
        }
        Some("--to") => client(&args[1..]),
        Some("--cli") => cli(&args[1..]),
        Some("--install") => install(),
        Some("--uninstall") => uninstall(),
        Some("--quit") => {
            let _ = ipc::send("quit", Duration::from_secs(2));
        }
        Some("--export-assets") => {
            // Utilisé par packaging/build.ps1
            let dir = PathBuf::from(args.get(1).map_or(".", String::as_str));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("bascule.ico"), icon::ico_file());
            for s in [44, 50, 150] {
                let _ = icon::write_png(&dir.join(format!("logo{s}.png")), s);
            }
        }
        _ => message_box(
            "Usage :\n  bascule.exe --install\n  bascule.exe --uninstall\n  \
             bascule.exe --cli png|jpg <fichiers ou dossiers>",
            false,
        ),
    }
}

fn running_from_install_dir() -> bool {
    let norm = |p: PathBuf| p.canonicalize().unwrap_or(p);
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    exe_dir.is_some_and(|d| norm(d) == norm(util::install_dir()))
}

fn message_box(text: &str, warn: bool) {
    let icon = if warn { MB_ICONWARNING } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("Bascule"), MB_OK | icon);
    }
}

fn absolute(p: &str) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p))
}

// ---------------------------------------------------------------- client (menu contextuel)

fn client(args: &[String]) {
    let Some(fmt) = args.first().and_then(|s| Format::parse(s)) else { return };
    let paths: Vec<PathBuf> = args[1..].iter().map(|p| absolute(p)).collect();
    if paths.is_empty() {
        return;
    }
    let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    let msg = format!("convert {}\n{}", fmt.ext(), list.join("\n"));
    if send_or_start(&msg) {
        return;
    }
    // Pas de tray joignable : on convertit nous-mêmes.
    let errors: Vec<String> = match Converter::new() {
        Ok(c) => paths
            .iter()
            .filter_map(|p| c.convert(p, fmt, registry::jpeg_quality()).err().map(|e| format!("{} : {e}", p.display())))
            .collect(),
        Err(e) => vec![e],
    };
    if !errors.is_empty() {
        message_box(&errors.join("\n"), true);
    }
}

/// Envoie au tray ; le démarre s'il ne tourne pas. Un seul des clients
/// lancés en rafale par l'Explorateur se charge du démarrage.
fn send_or_start(msg: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut tried_spawn = false;
    loop {
        match ipc::send(msg, Duration::from_secs(2)) {
            Ok(()) => return true,
            Err(_) if Instant::now() >= deadline => return false,
            Err(_) => {
                if !tried_spawn {
                    tried_spawn = true;
                    if !server::is_running() && take_spawn_lock() {
                        spawn_tray(&std::env::current_exe().unwrap_or_default());
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn take_spawn_lock() -> bool {
    unsafe {
        // Le handle est volontairement gardé jusqu'à la fin du process.
        let h = CreateMutexW(None, false, w!("Local\\Bascule-spawn"));
        h.is_ok() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

fn spawn_tray(exe: &Path) {
    let _ = Command::new(exe)
        .arg("--tray")
        .creation_flags((DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).0)
        .spawn();
}

// ---------------------------------------------------------------- CLI (tests / scripts)

fn cli(args: &[String]) {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let mut out = std::io::stdout();
    let Some(fmt) = args.first().and_then(|s| Format::parse(s)) else {
        let _ = writeln!(out, "usage : bascule --cli png|jpg <fichiers ou dossiers>");
        return;
    };
    let mut files = Vec::new();
    for a in &args[1..] {
        let p = absolute(a);
        if p.is_dir() {
            for e in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                let path = e.path();
                let ext = path.extension().map(|x| format!(".{}", x.to_string_lossy().to_lowercase()));
                if ext.is_some_and(|x| SOURCES.iter().any(|(s, _)| *s == x)) {
                    files.push(path);
                }
            }
        } else {
            files.push(p);
        }
    }
    let quality = registry::jpeg_quality();
    let threads = server::worker_count().min(files.len().max(1));
    let start = Instant::now();
    let next = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let out = Mutex::new(out);
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let conv = Converter::new();
                if std::env::var_os("BASCULE_WARMUP").is_some() {
                    if let Ok(c) = &conv {
                        let t = Instant::now();
                        c.warm_up();
                        let _ = writeln!(out.lock().unwrap(), "warm-up {}", util::fmt_duration(t.elapsed()));
                    }
                }
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(f) = files.get(i) else { break };
                    let t = Instant::now();
                    let r = conv.as_ref().map_err(Clone::clone).and_then(|c| c.convert(f, fmt, quality));
                    let name = f.file_name().unwrap_or_default().to_string_lossy();
                    let line = match r {
                        Ok(o) => format!(
                            "ok    {name} -> {}  ({})",
                            o.file_name().unwrap_or_default().to_string_lossy(),
                            util::fmt_duration(t.elapsed())
                        ),
                        Err(e) => {
                            failures.fetch_add(1, Ordering::Relaxed);
                            format!("ERREUR {name} : {e}")
                        }
                    };
                    let _ = writeln!(out.lock().unwrap(), "{line}");
                }
            });
        }
    });
    let _ = writeln!(
        out.lock().unwrap(),
        "{} fichier(s), {} échec(s), {} threads, total {}",
        files.len(),
        failures.load(Ordering::Relaxed),
        threads,
        util::fmt_duration(start.elapsed())
    );
}

// ---------------------------------------------------------------- installation

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
        _ => false,
    }
}

fn stop_tray() {
    if server::is_running() {
        let _ = ipc::send("quit", Duration::from_secs(2));
        let deadline = Instant::now() + Duration::from_secs(12);
        while server::is_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Fichiers du menu Windows 11, produits par packaging/build.ps1 à côté de l'exe.
const SHELL_DLL: &str = "bascule_shell.dll";
const SHELL_MSIX: &str = "Bascule.msix";
const SHELL_CER: &str = "bascule.cer";
static REGISTER_PS1: &str = include_str!("../packaging/register.ps1");

/// Lance packaging/register.ps1 (embarqué). Retourne le message d'erreur éventuel.
fn run_register(action: &str, dir: &Path) -> Result<(), String> {
    let script = std::env::temp_dir().join(format!("bascule-register-{}.ps1", std::process::id()));
    // BOM UTF-8 : Windows PowerShell 5.1 lirait sinon le script en ANSI.
    std::fs::write(&script, format!("\u{FEFF}{REGISTER_PS1}")).map_err(|e| e.to_string())?;
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .args(["-Action", action, "-Dir"])
        .arg(dir)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
    let _ = std::fs::remove_file(&script);
    let out = out.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("code {}", out.status) } else { err })
    }
}

fn copy_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut r = std::fs::copy(from, to).map(|_| ());
    for _ in 0..30 {
        if r.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(150));
        r = std::fs::copy(from, to).map(|_| ());
    }
    r
}

fn install() {
    let dir = util::install_dir();
    let dest = dir.join("bascule.exe");
    let me = std::env::current_exe().unwrap_or_default();
    let src_dir = me.parent().map(Path::to_path_buf).unwrap_or_default();
    let win11 = registry::is_windows_11();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return message_box(&format!("Impossible de créer {} : {e}", dir.display()), true);
    }
    if !same_path(&me, &dest) {
        stop_tray();
        if win11 && registry::shell_package_installed() {
            // Retirer le package arrête le dllhost qui verrouille la DLL.
            let _ = run_register("uninstall", &dir);
        }
        let mut files = vec![(me.clone(), dest.clone())];
        for f in [SHELL_DLL, SHELL_MSIX, SHELL_CER] {
            if src_dir.join(f).exists() {
                files.push((src_dir.join(f), dir.join(f)));
            }
        }
        for (from, to) in files {
            if let Err(e) = copy_with_retry(&from, &to) {
                return message_box(&format!("Copie vers {} impossible : {e}", to.display()), true);
            }
        }
    }
    let ico = icon::ensure_file();
    if let Err(e) = registry::set_autostart(Some(&dest)) {
        return message_box(&format!("Écriture du registre impossible : {}", e.message()), true);
    }

    // Windows 11 : menu principal via le package sparse ; sinon (ou en cas d'échec) menu classique.
    let has_package = [SHELL_DLL, SHELL_MSIX, SHELL_CER].iter().all(|f| dir.join(f).exists());
    let mut fallback_reason = None;
    if win11 && has_package {
        match run_register("install", &dir) {
            Ok(()) => {
                registry::uninstall_context_menu();
                registry::set_shell_package(true);
            }
            Err(e) => fallback_reason = Some(e),
        }
    } else if win11 {
        fallback_reason = Some(format!("fichiers {SHELL_MSIX} / {SHELL_DLL} absents (lancer packaging\\build.ps1)"));
    }
    let packaged = win11 && fallback_reason.is_none();
    if !packaged {
        registry::set_shell_package(false);
        if let Err(e) = registry::install_context_menu(&dest, &ico) {
            return message_box(&format!("Écriture du registre impossible : {}", e.message()), true);
        }
    }
    spawn_tray(&dest);

    let mut text = String::from(
        "Bascule est installé et tourne dans la zone de notification.\n\n\
         Clic droit sur une ou plusieurs images HEIC → « Convertir... » → PNG ou JPG.",
    );
    if let Some(reason) = fallback_reason {
        text += &format!(
            "\n\nLe menu principal de Windows 11 n'a pas pu être installé :\n{reason}\n\n\
             L'entrée est disponible dans « Afficher plus d'options » (ou Maj + clic droit)."
        );
    }
    message_box(&text, !packaged && win11);
}

fn uninstall() {
    stop_tray();
    if registry::is_windows_11() {
        let _ = run_register("uninstall", &util::install_dir());
    }
    registry::uninstall_context_menu();
    let _ = registry::set_autostart(None);
    registry::remove_settings();
    let dir = util::install_dir();
    let me = std::env::current_exe().unwrap_or_default();
    if me.starts_with(&dir) || same_path(me.parent().unwrap_or(&me), &dir) {
        // On ne peut pas supprimer l'exe en cours d'exécution : suppression différée.
        let _ = Command::new("cmd.exe")
            .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn();
    } else {
        let _ = std::fs::remove_dir_all(&dir);
    }
    message_box("Bascule a été désinstallé.", false);
}
