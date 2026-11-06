//! Processus résident : file de travaux, pool de workers, suivi des lots.

use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

use crate::convert::Converter;
use crate::formats::Format;
use crate::{ipc, registry, tray};

const MUTEX_NAME: windows::core::PCWSTR = w!("Local\\Bascule-tray");

pub struct Job {
    pub src: PathBuf,
    pub fmt: Format,
}

/// Un « lot » = toutes les conversions arrivées sans temps mort (ex. 40 fichiers
/// sélectionnés dans l'Explorateur = 40 process clients, mais une seule notification).
#[derive(Default)]
pub struct Batch {
    pub pending: usize,
    pub total: usize,
    pub ok: usize,
    pub failed: Vec<(PathBuf, String)>,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
    pub last_output: Option<PathBuf>,
}

pub struct Shared {
    pub batch: Mutex<Batch>,
    pub hwnd: AtomicIsize,
    pub jpeg_quality: AtomicU32,
}

impl Shared {
    fn post(&self, msg: u32) {
        let h = self.hwnd.load(Ordering::Acquire);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(HWND(h as _), msg, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn enqueue(&self, n: usize) {
        let mut b = self.batch.lock().unwrap();
        if b.started.is_none() {
            b.started = Some(Instant::now());
        }
        b.pending += n;
        b.total += n;
        drop(b);
        self.post(tray::WM_JOB_PROGRESS);
    }

    fn done(&self, src: PathBuf, result: Result<PathBuf, String>) {
        let mut b = self.batch.lock().unwrap();
        b.pending -= 1;
        match result {
            Ok(out) => {
                b.ok += 1;
                b.last_output = Some(out);
            }
            Err(e) => b.failed.push((src, e)),
        }
        let idle = b.pending == 0;
        if idle {
            b.finished = Some(Instant::now());
        }
        drop(b);
        self.post(if idle { tray::WM_JOB_IDLE } else { tray::WM_JOB_PROGRESS });
    }

    /// Récupère le lot s'il est terminé (et repart de zéro).
    pub fn take_finished(&self) -> Option<Batch> {
        let mut b = self.batch.lock().unwrap();
        (b.pending == 0 && b.started.is_some()).then(|| std::mem::take(&mut *b))
    }

    /// (terminés, total) du lot en cours.
    pub fn progress(&self) -> (usize, usize) {
        let b = self.batch.lock().unwrap();
        (b.total - b.pending, b.total)
    }
}

pub fn is_running() -> bool {
    unsafe {
        match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, MUTEX_NAME) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

pub fn worker_count() -> usize {
    if let Some(n) = std::env::var("BASCULE_THREADS").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    // Le décodeur HEVC de Windows est déjà multithreadé : au-delà de 4 on ne gagne
    // plus rien (mesuré sur Ryzen 5 7600X), on ne fait que consommer de la mémoire.
    thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 4)
}

pub fn run() {
    // Instance unique par session utilisateur.
    let _mutex = unsafe { CreateMutexW(None, false, MUTEX_NAME) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    let shared = Arc::new(Shared {
        batch: Mutex::new(Batch::default()),
        hwnd: AtomicIsize::new(0),
        jpeg_quality: AtomicU32::new(registry::jpeg_quality()),
    });

    let (tx, rx) = mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..worker_count() {
        let rx = rx.clone();
        let shared = shared.clone();
        thread::spawn(move || {
            let conv = Converter::new();
            if let Ok(c) = &conv {
                c.warm_up();
            }
            loop {
                let job = match rx.lock().unwrap().recv() {
                    Ok(j) => j,
                    Err(_) => return,
                };
                let q = shared.jpeg_quality.load(Ordering::Relaxed);
                let r = match &conv {
                    Ok(c) => c.convert(&job.src, job.fmt, q),
                    Err(e) => Err(e.clone()),
                };
                shared.done(job.src, r);
            }
        });
    }

    // Écoute du pipe : démarrée avant la fenêtre pour accepter les clients au plus tôt.
    {
        let shared = shared.clone();
        thread::spawn(move || {
            let _ = ipc::serve(|msg| handle(&msg, &tx, &shared));
        });
    }

    tray::run(shared.clone());

    // Quitter : on laisse les conversions en cours se terminer (10 s max).
    let deadline = Instant::now() + Duration::from_secs(10);
    while shared.progress().0 < shared.progress().1 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    std::process::exit(0);
}

fn handle(msg: &str, tx: &mpsc::Sender<Job>, shared: &Shared) {
    let mut lines = msg.lines();
    let cmd = lines.next().unwrap_or_default();
    if cmd == "quit" {
        shared.post(WM_CLOSE);
    } else if let Some(fmt) = cmd.strip_prefix("convert ").and_then(Format::parse) {
        let paths: Vec<PathBuf> = lines.filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect();
        shared.enqueue(paths.len());
        for src in paths {
            let _ = tx.send(Job { src, fmt });
        }
    }
}
