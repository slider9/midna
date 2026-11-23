//! Small Windows helpers: wide strings, processes, the control pipe's I/O and its access rules.
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, GetLastError, HANDLE, HLOCAL, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

pub fn last_error() -> io::Error {
    io::Error::last_os_error()
}

/// A handle from a call that returns null on failure.
pub fn owned(h: HANDLE) -> io::Result<OwnedHandle> {
    if h.is_null() || h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    // SAFETY: a handle this process just opened and nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(h) })
}

pub fn raw(h: &OwnedHandle) -> HANDLE {
    h.as_raw_handle()
}

/// The program `pid` runs, if it's running and this user may ask.
pub fn process_image(pid: u32) -> Option<PathBuf> {
    // SAFETY: plain calls on a handle we own; the buffer's size goes with it.
    unsafe {
        let h = owned(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)).ok()?;
        let mut buf = vec![0u16; 32768];
        let mut n = buf.len() as u32;
        if QueryFullProcessImageNameW(raw(&h), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut n) == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])))
    }
}

/// One end of a control pipe, opened for overlapped I/O. A pipe handle opened for ordinary
/// (synchronous) I/O runs one call at a time, so a read waiting for the next request would hold
/// up the keeper's `exited`; overlapped, a read and a write can wait at once from two threads.
pub struct Pipe {
    h: OwnedHandle,
}

impl Pipe {
    pub fn new(h: OwnedHandle) -> Pipe {
        Pipe { h }
    }

    pub fn handle(&self) -> HANDLE {
        raw(&self.h)
    }

    /// One read or write, waited for. Ok(0) from a read: the other end is gone.
    fn io(&self, read: bool, ptr: *mut u8, len: usize) -> io::Result<usize> {
        let len = len.min(u32::MAX as usize) as u32;
        // SAFETY: the buffer and the OVERLAPPED outlive the call, which is waited for here.
        unsafe {
            let ev = owned(CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()))?;
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = raw(&ev);
            let ok = if read {
                ReadFile(self.handle(), ptr, len, std::ptr::null_mut(), &mut ov)
            } else {
                WriteFile(self.handle(), ptr, len, std::ptr::null_mut(), &mut ov)
            };
            if ok == 0 && GetLastError() != ERROR_IO_PENDING {
                return gone(read);
            }
            let mut n = 0u32;
            if GetOverlappedResult(self.handle(), &ov, &mut n, 1) == 0 {
                return gone(read);
            }
            Ok(n as usize)
        }
    }
}

fn gone(read: bool) -> io::Result<usize> {
    let e = unsafe { GetLastError() };
    if matches!(e, ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA) {
        if read {
            return Ok(0);
        }
        return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the other end of the pipe is gone"));
    }
    Err(io::Error::from_raw_os_error(e as i32))
}

impl io::Read for &Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.io(true, buf.as_mut_ptr(), buf.len())
    }
}

impl io::Write for &Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.io(false, buf.as_ptr() as *mut u8, buf.len())? {
            0 => Err(io::Error::new(io::ErrorKind::WriteZero, "pipe took nothing")),
            n => Ok(n),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Access for the current user only, nobody else (not even administrators or SYSTEM), for the
/// control pipe.
pub struct UserOnly {
    sd: PSECURITY_DESCRIPTOR,
    pub attrs: SECURITY_ATTRIBUTES,
}

impl Drop for UserOnly {
    fn drop(&mut self) {
        unsafe { LocalFree(self.sd as HLOCAL) };
    }
}

pub fn user_only() -> io::Result<UserOnly> {
    let sid = user_sid()?;
    // Protected (P): nothing inherited; one entry, full access for the user.
    let sddl = wide(format!("D:P(A;;GA;;;{sid})"));
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-terminated string; the descriptor is freed with LocalFree on drop.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
        return Err(last_error());
    }
    let attrs = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
    Ok(UserOnly { sd, attrs })
}

/// The current user's SID, as `S-1-5-…`.
fn user_sid() -> io::Result<String> {
    // SAFETY: the token buffer is sized by the first call; the string is freed with LocalFree.
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(last_error());
        }
        let token = owned(token)?;
        let mut len = 0u32;
        GetTokenInformation(raw(&token), TokenUser, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if GetTokenInformation(raw(&token), TokenUser, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
            return Err(last_error());
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
            return Err(last_error());
        }
        let n = (0..).take_while(|&i| *s.add(i) != 0).count();
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
        LocalFree(s as HLOCAL);
        Ok(out)
    }
}

/// The process that started this one, held open so its pid can't go to another process while
/// we look at it: None if it has exited already, or a process with its pid started after us
/// (the pid was reused).
pub struct Creator {
    pub pid: u32,
    _process: OwnedHandle,
}

impl Creator {
    pub fn find() -> Option<Creator> {
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
        use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
        let me = std::process::id();
        // SAFETY: a snapshot we own, walked with a sized entry; plain calls on handles we own.
        unsafe {
            let snap = owned(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)).ok()?;
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(raw(&snap), &mut e) != 0;
            let mut parent = None;
            while ok {
                if e.th32ProcessID == me {
                    parent = Some(e.th32ParentProcessID);
                    break;
                }
                ok = Process32NextW(raw(&snap), &mut e) != 0;
            }
            let pid = parent.filter(|&p| p != 0)?;
            let process = owned(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid)).ok()?;
            // A parent pid isn't kept up to date: a process that started after us under that
            // pid isn't the one that started us.
            if started(raw(&process))? > started(GetCurrentProcess())? {
                return None;
            }
            Some(Creator { pid, _process: process })
        }
    }

}

/// When the process `h` started (FILETIME ticks).
fn started(h: HANDLE) -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let z = || FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut x, mut k, mut u) = (z(), z(), z(), z());
    if unsafe { GetProcessTimes(h, &mut c, &mut x, &mut k, &mut u) } == 0 {
        return None;
    }
    Some((c.dwHighDateTime as u64) << 32 | c.dwLowDateTime as u64)
}
