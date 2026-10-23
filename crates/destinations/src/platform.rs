//! Print, email and open-link per OS.

use std::path::Path;

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Hand the file to the system's "print" verb (the image viewer's print dialog).
#[cfg(windows)]
pub fn print(path: &Path) -> Result<(), String> {
    shell_execute("print", &path.to_string_lossy())
}

#[cfg(windows)]
pub fn open_url(url: &str) -> Result<(), String> {
    shell_execute("open", url)
}

#[cfg(windows)]
fn shell_execute(verb: &str, target: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;
    let (v, t) = (wide(verb), wide(target));
    let code = unsafe { ShellExecuteW(None, PCWSTR(v.as_ptr()), PCWSTR(t.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // ShellExecute returns a value above 32 on success.
    if code.0 as isize > 32 { Ok(()) } else { Err(format!("Windows could not {verb} {target} (code {})", code.0 as isize)) }
}

/// Open the default mail app's compose window with the file attached, through
/// Simple MAPI. Some mail apps keep the call open until the email window
/// closes, so it runs on its own thread: a quick error means no MAPI mail app,
/// no answer within a moment means the compose window is up.
#[cfg(windows)]
pub fn email(path: &Path, subject: &str) -> Result<(), String> {
    use std::sync::mpsc;
    use std::time::Duration;
    let (path, subject) = (path.to_string_lossy().into_owned(), subject.to_string());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(mapi_send(&path, &subject));
    });
    match rx.recv_timeout(Duration::from_millis(1500)) {
        Ok(result) => result,
        Err(_) => Ok(()),
    }
}

#[cfg(windows)]
fn mapi_send(path: &str, subject: &str) -> Result<(), String> {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::Mapi::{LPMAPISENDMAILW, MAPI_DIALOG, MAPI_LOGON_UI, MAPI_USER_ABORT, MapiFileDescW, MapiMessageW, SUCCESS_SUCCESS};
    use windows::core::{PWSTR, s, w};
    unsafe {
        let lib = LoadLibraryW(w!("mapi32.dll")).map_err(|e| format!("no MAPI: {e}"))?;
        let proc = GetProcAddress(lib, s!("MAPISendMailW")).ok_or("no MAPISendMailW")?;
        let send: LPMAPISENDMAILW = std::mem::transmute(proc);
        let send = send.ok_or("no MAPISendMailW")?;
        let mut file_path = wide(path);
        let mut file_name = wide(Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_str());
        let mut subj = wide(subject);
        let mut file = MapiFileDescW { nPosition: u32::MAX, lpszPathName: PWSTR(file_path.as_mut_ptr()), lpszFileName: PWSTR(file_name.as_mut_ptr()), ..Default::default() };
        let msg = MapiMessageW { lpszSubject: PWSTR(subj.as_mut_ptr()), nFileCount: 1, lpFiles: &mut file, ..Default::default() };
        match send(0, 0, &msg, MAPI_DIALOG | MAPI_LOGON_UI, 0) {
            SUCCESS_SUCCESS | MAPI_USER_ABORT => Ok(()),
            code => Err(format!("MAPI error {code}")),
        }
    }
}

#[cfg(not(windows))]
pub fn print(path: &Path) -> Result<(), String> {
    let cmd = if cfg!(target_os = "macos") { "lpr" } else { "lp" };
    std::process::Command::new(cmd).arg(path).status().map_err(|e| format!("could not run {cmd}: {e}")).and_then(|s| if s.success() { Ok(()) } else { Err(format!("{cmd} failed")) })
}

#[cfg(not(windows))]
pub fn email(_path: &Path, _subject: &str) -> Result<(), String> {
    Err("attachments need a MAPI mail app".into())
}

#[cfg(not(windows))]
pub fn open_url(url: &str) -> Result<(), String> {
    let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(cmd).arg(url).spawn().map(|_| ()).map_err(|e| format!("could not run {cmd}: {e}"))
}
