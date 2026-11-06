//! Icône dans la zone de notification, menu et notifications de fin de lot.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::server::{Batch, Shared};
use crate::util::{fill_wide, fmt_duration};
use crate::{icon, registry};

pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_JOB_PROGRESS: u32 = WM_APP + 2;
pub const WM_JOB_IDLE: u32 = WM_APP + 3;

const NIN_KEYSELECT: u32 = NIN_SELECT | 1; // NIN_SELECT | NINF_KEY
const TIMER_IDLE: usize = 1;
/// Attente après le dernier fichier avant de notifier : les process lancés par
/// l'Explorateur arrivent en rafale, on évite une notification par fichier.
const IDLE_DELAY_MS: u32 = 400;

const ID_CONTEXT_MENU: usize = 10;
const ID_AUTOSTART: usize = 11;
const ID_QUIT: usize = 12;
const ID_QUALITY: usize = 100; // + valeur
const QUALITIES: [u32; 5] = [75, 85, 90, 95, 100];

struct App {
    shared: Arc<Shared>,
    hicon: isize,
    taskbar_created: u32,
    last_tip: Mutex<Instant>,
    last_output: Mutex<Option<PathBuf>>,
}

static APP: OnceLock<App> = OnceLock::new();

pub fn run(shared: Arc<Shared>) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("ConvertisseurTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Convertisseur"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            hinst,
            None,
        ) else {
            return;
        };

        let size = GetSystemMetrics(SM_CXSMICON);
        let hicon = CreateIconFromResourceEx(&icon::dib(size as u32), TRUE, 0x0003_0000, size, size, LR_DEFAULTCOLOR)
            .map(|h| h.0 as isize)
            .unwrap_or(0);

        let _ = APP.set(App {
            shared: shared.clone(),
            hicon,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            last_tip: Mutex::new(Instant::now() - Duration::from_secs(1)),
            last_output: Mutex::new(None),
        });
        shared.hwnd.store(hwnd.0 as isize, Ordering::Release);
        add_icon(hwnd);
        // Des travaux ont pu arriver avant la création de la fenêtre.
        let _ = PostMessageW(hwnd, WM_JOB_PROGRESS, WPARAM(0), LPARAM(0));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        shared.hwnd.store(0, Ordering::Release);
    }
}

fn base_nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

unsafe fn add_icon(hwnd: HWND) {
    let Some(app) = APP.get() else { return };
    let mut n = base_nid(hwnd);
    n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    n.uCallbackMessage = WM_TRAY;
    n.hIcon = HICON(app.hicon as _);
    fill_wide(&mut n.szTip, idle_tip());
    let _ = Shell_NotifyIconW(NIM_ADD, &n);
    n.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    let _ = Shell_NotifyIconW(NIM_SETVERSION, &n);
}

fn idle_tip() -> &'static str {
    "Convertisseur — HEIC → PNG / JPG"
}

unsafe fn set_tip(hwnd: HWND, tip: &str) {
    let mut n = base_nid(hwnd);
    n.uFlags = NIF_TIP | NIF_SHOWTIP;
    fill_wide(&mut n.szTip, tip);
    let _ = Shell_NotifyIconW(NIM_MODIFY, &n);
}

unsafe fn update_progress(hwnd: HWND, app: &App) {
    // Limité à ~10 rafraîchissements / s.
    let mut last = app.last_tip.lock().unwrap();
    if last.elapsed() < Duration::from_millis(100) {
        return;
    }
    *last = Instant::now();
    let (done, total) = app.shared.progress();
    if done < total {
        set_tip(hwnd, &format!("Conversion : {done} / {total}"));
    }
}

unsafe fn notify(hwnd: HWND, app: &App, b: &Batch) {
    set_tip(hwnd, idle_tip());
    let dur = match (b.started, b.finished) {
        (Some(s), Some(f)) => fmt_duration(f - s),
        _ => String::new(),
    };
    let file_name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (title, text, warn) = if b.failed.is_empty() {
        let text = match (&b.last_output, b.ok) {
            (Some(p), 1) => format!("{} créé en {dur}", file_name(p)),
            _ => format!("{} fichiers convertis en {dur}", b.ok),
        };
        ("Conversion terminée".to_string(), text, false)
    } else {
        let (src, err) = &b.failed[0];
        let mut text = format!("{} : {err}", file_name(src));
        if b.failed.len() > 1 || b.ok > 0 {
            text = format!("{} réussi(s), {} échec(s)\n{text}", b.ok, b.failed.len());
        }
        let title = if b.ok == 0 { "Échec de la conversion" } else { "Conversion terminée avec des erreurs" };
        (title.to_string(), text, true)
    };
    *app.last_output.lock().unwrap() = b.last_output.clone();

    let mut n = base_nid(hwnd);
    n.uFlags = NIF_INFO;
    fill_wide(&mut n.szInfoTitle, &title);
    fill_wide(&mut n.szInfo, &text);
    n.dwInfoFlags = if warn { NIIF_WARNING } else { NIIF_INFO } | NIIF_NOSOUND;
    let _ = Shell_NotifyIconW(NIM_MODIFY, &n);
}

/// Clic sur la notification : ouvre l'Explorateur sur le dernier fichier créé.
fn reveal_last(app: &App) {
    if let Some(p) = app.last_output.lock().unwrap().clone() {
        let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn();
    }
}

unsafe fn show_menu(hwnd: HWND, app: &App) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
    let installed = registry::context_menu_installed();
    let autostart = registry::autostart_enabled();
    let quality = app.shared.jpeg_quality.load(Ordering::Relaxed);

    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("Convertisseur · HEIC → PNG / JPG"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    if registry::shell_package_installed() {
        // Géré par le package Windows 11 : se désinstalle avec --uninstall.
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED | MF_CHECKED, 0, w!("Menu contextuel Windows 11"));
    } else {
        let label = w!("Menu contextuel « Convertir... »");
        let _ = AppendMenuW(menu, MF_STRING | check(installed), ID_CONTEXT_MENU, label);
    }
    let _ = AppendMenuW(menu, MF_STRING | check(autostart), ID_AUTOSTART, w!("Lancer au démarrage de Windows"));
    if let Ok(sub) = CreatePopupMenu() {
        for q in QUALITIES {
            let label = HSTRING::from(format!("{q} %"));
            let _ = AppendMenuW(sub, MF_STRING | check(q == quality), ID_QUALITY + q as usize, &label);
        }
        let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Qualité JPG"));
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quitter"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    let exe = std::env::current_exe().unwrap_or_default();
    match cmd.0 as usize {
        ID_CONTEXT_MENU if installed => registry::uninstall_context_menu(),
        ID_CONTEXT_MENU => {
            let _ = registry::install_context_menu(&exe, &icon::ensure_file());
        }
        ID_AUTOSTART => {
            let _ = registry::set_autostart((!autostart).then_some(exe.as_path()));
        }
        ID_QUIT => {
            let _ = DestroyWindow(hwnd);
        }
        id if id > ID_QUALITY && id <= ID_QUALITY + 100 => {
            let q = (id - ID_QUALITY) as u32;
            app.shared.jpeg_quality.store(q, Ordering::Relaxed);
            registry::set_jpeg_quality(q);
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(app) = APP.get() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        WM_TRAY => match (lp.0 as u32) & 0xFFFF {
            WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT => show_menu(hwnd, app),
            NIN_BALLOONUSERCLICK => reveal_last(app),
            _ => {}
        },
        WM_JOB_PROGRESS => update_progress(hwnd, app),
        WM_JOB_IDLE => {
            SetTimer(hwnd, TIMER_IDLE, IDLE_DELAY_MS, None);
        }
        WM_TIMER if wp.0 == TIMER_IDLE => {
            let _ = KillTimer(hwnd, TIMER_IDLE);
            if let Some(batch) = app.shared.take_finished() {
                notify(hwnd, app, &batch);
            }
        }
        WM_DESTROY => {
            let _ = Shell_NotifyIconW(NIM_DELETE, &base_nid(hwnd));
            PostQuitMessage(0);
        }
        m if m == app.taskbar_created => add_icon(hwnd), // Explorer redémarré
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}
