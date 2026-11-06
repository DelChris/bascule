//! Communication client (menu contextuel) -> serveur (tray) par named pipe.
//! Protocole : un message texte UTF-8 par connexion, le client écrit puis ferme.
//!   "convert png\nC:\\a.heic\nC:\\b.heic"   |   "quit"

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
use windows::Win32::System::Pipes::*;

const ERROR_PIPE_BUSY: i32 = 231;

pub fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    format!(r"\\.\pipe\convertisseur-v1-{user}")
}

fn create_instance(name: &HSTRING, first: bool) -> windows::core::Result<HANDLE> {
    let mut mode = PIPE_ACCESS_INBOUND;
    if first {
        mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            64 * 1024,
            0,
            None,
        )
    };
    if h.is_invalid() {
        Err(windows::core::Error::from_win32())
    } else {
        Ok(h)
    }
}

/// Boucle d'écoute (bloquante). La prochaine instance est créée avant de lire
/// le message courant, pour que les clients simultanés ne tombent jamais dans le vide.
pub fn serve(mut on_message: impl FnMut(String)) -> windows::core::Result<()> {
    let name = HSTRING::from(pipe_name());
    let mut h = create_instance(&name, true)?;
    loop {
        let connected = unsafe { ConnectNamedPipe(h, None) };
        let ok = connected.is_ok()
            || connected.as_ref().err().map(|e| e.code()) == Some(ERROR_PIPE_CONNECTED.to_hresult());
        let next = create_instance(&name, false)?;
        if ok {
            // File prend possession du handle et le ferme à la fin.
            let mut file = unsafe { File::from_raw_handle(h.0) };
            let mut buf = Vec::new();
            let _ = file.read_to_end(&mut buf);
            if !buf.is_empty() {
                on_message(String::from_utf8_lossy(&buf).into_owned());
            }
        } else {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        h = next;
    }
}

/// Envoie un message. Erreur `NotFound` si aucun serveur n'écoute.
pub fn send(msg: &str, timeout: Duration) -> io::Result<()> {
    let name = pipe_name();
    let start = Instant::now();
    loop {
        match OpenOptions::new().write(true).open(&name) {
            Ok(mut f) => return f.write_all(msg.as_bytes()),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && start.elapsed() < timeout => {
                unsafe {
                    let _ = WaitNamedPipeW(&HSTRING::from(name.as_str()), 200);
                }
            }
            Err(e) => return Err(e),
        }
    }
}
