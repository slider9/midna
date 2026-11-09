//! The Windows process table: a ToolHelp snapshot for pids, parents and names, and a process
//! handle for the rest (start time, CPU, memory, command line, working directory).
//!
//! Names drop `.exe`, so `node.exe` is `node` like on macOS and the name rules (launchers,
//! servers, apps) apply unchanged. Windows has no process groups: a process's pgid is its pid.
use super::Bsd;
use std::collections::HashMap;
use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_NAME_WIN32, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_VM_READ, QueryFullProcessImageNameW, SetPriorityClass,
};

/// `NtQueryInformationProcess` class for the command line (Windows 8.1+).
const PROCESS_COMMAND_LINE_INFORMATION: i32 = 60;

struct Handle(HANDLE);
impl Handle {
    fn open(pid: i32, access: u32) -> Option<Handle> {
        let h = unsafe { OpenProcess(access, 0, pid as u32) };
        (!h.is_null()).then_some(Handle(h))
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// pid -> (parent pid, name without `.exe`), from one ToolHelp snapshot.
pub fn snapshot() -> HashMap<i32, (i32, String)> {
    let mut out = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE || snap.is_null() {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let exe = String::from_utf16_lossy(&e.szExeFile[..len]);
            out.insert(e.th32ProcessID as i32, (e.th32ParentProcessID as i32, strip_exe(&exe)));
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    out
}

fn strip_exe(name: &str) -> String {
    match name.len().checked_sub(4) {
        Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => name[..i].to_string(),
        _ => name.to_string(),
    }
}

fn filetime_unix(ft: &FILETIME) -> i64 {
    let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    (ticks / 10_000_000) as i64 - 11_644_473_600
}

/// (creation, kernel + user CPU in 100ns ticks).
fn times(pid: i32) -> Option<(i64, u64)> {
    let h = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    if unsafe { GetProcessTimes(h.0, &mut c, &mut e, &mut k, &mut u) } == 0 {
        return None;
    }
    let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    Some((filetime_unix(&c), ticks(k) + ticks(u)))
}

pub fn start_time(pid: i32) -> Option<i64> {
    times(pid).map(|(s, _)| s)
}

pub fn all_pids() -> Vec<i32> {
    snapshot().into_keys().filter(|&p| p > 0).collect()
}

/// Every process, with what [`Bsd`] holds (one snapshot; a start time per process). The System
/// Idle Process (pid 0) and processes we may not open keep start 0.
pub(super) fn all_bsd() -> Vec<(i32, Bsd)> {
    snapshot()
        .into_iter()
        .filter(|(pid, _)| *pid > 0)
        .map(|(pid, (ppid, name))| (pid, Bsd { ppid, pgid: pid, name, start: start_time(pid).unwrap_or(0) }))
        .collect()
}

pub(super) fn bsd_info(pid: i32) -> Option<Bsd> {
    let (ppid, name) = snapshot().remove(&pid)?;
    Some(Bsd { ppid, pgid: pid, name, start: start_time(pid).unwrap_or(0) })
}

/// The full path of a process's executable.
pub fn exe_path(pid: i32) -> Option<String> {
    let h = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    if unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Windows' closest thing to macOS's background mode: the idle priority class (it gets the CPU
/// only when nothing else wants it). Children started later inherit it.
pub fn set_background(pid: i32, on: bool) -> bool {
    let Some(h) = Handle::open(pid, PROCESS_SET_INFORMATION) else { return false };
    unsafe { SetPriorityClass(h.0, if on { IDLE_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS }) != 0 }
}

/// The command line the process was started with, as one string (quotes included).
pub fn command_line(pid: i32) -> Option<String> {
    let h = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut buf = vec![0u64; 512];
    for _ in 0..3 {
        let mut len = 0u32;
        let st = unsafe { NtQueryInformationProcess(h.0, PROCESS_COMMAND_LINE_INFORMATION, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len) };
        if st >= 0 {
            // A UNICODE_STRING (u16 length in bytes, u16 max, pointer) followed by the text.
            let bytes = (buf[0] & 0xffff) as usize;
            let text = unsafe { std::slice::from_raw_parts((buf[1] as usize) as *const u16, bytes / 2) };
            let s = String::from_utf16_lossy(text);
            return (!s.is_empty()).then_some(s);
        }
        if len as usize <= buf.len() * 8 {
            return None;
        }
        buf = vec![0u64; (len as usize).div_ceil(8)];
    }
    None
}

/// A process's working directory, read from its process parameters (offsets for 64-bit
/// processes: PEB.ProcessParameters at 0x20, CurrentDirectory.DosPath at 0x38).
pub fn cwd(pid: i32) -> Option<String> {
    let h = Handle::open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
    let mut pbi: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let mut len = 0u32;
    let st = unsafe { NtQueryInformationProcess(h.0, ProcessBasicInformation, &mut pbi as *mut _ as *mut _, std::mem::size_of_val(&pbi) as u32, &mut len) };
    if st < 0 || pbi.PebBaseAddress.is_null() {
        return None;
    }
    let read = |addr: usize, out: &mut [u8]| -> bool {
        let mut got = 0usize;
        unsafe { ReadProcessMemory(h.0, addr as *const _, out.as_mut_ptr().cast(), out.len(), &mut got) != 0 && got == out.len() }
    };
    let mut ptr = [0u8; 8];
    if !read(pbi.PebBaseAddress as usize + 0x20, &mut ptr) {
        return None;
    }
    let params = usize::from_ne_bytes(ptr);
    let mut us = [0u8; 16];
    if params == 0 || !read(params + 0x38, &mut us) {
        return None;
    }
    let bytes = u16::from_ne_bytes([us[0], us[1]]) as usize;
    let buf = usize::from_ne_bytes(us[8..16].try_into().ok()?);
    if bytes == 0 || bytes > 65_534 || buf == 0 {
        return None;
    }
    let mut raw = vec![0u8; bytes];
    if !read(buf, &mut raw) {
        return None;
    }
    let wide: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_ne_bytes([c[0], c[1]])).collect();
    let mut s = String::from_utf16_lossy(&wide);
    // `C:\Users\me\` -> `C:\Users\me`, but `C:\` stays.
    if s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    std::path::Path::new(&s).is_absolute().then_some(s)
}

/// Private bytes: memory the process alone owns (Task Manager's "Memory" column).
pub fn footprint(pid: i32) -> Option<u64> {
    let h = Handle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    let mut c: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(h.0, &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb) };
    (ok != 0).then_some(c.PrivateUsage as u64)
}

/// CPU time a process has used (user + kernel), in nanoseconds.
pub fn cpu_ns(pid: i32) -> Option<u64> {
    times(pid).map(|(_, t)| t.saturating_mul(100))
}

/// Pids with a TCP socket listening on `port` (IPv4 or IPv6, any address).
pub fn listening_pids(port: u16) -> Vec<i32> {
    let mut pids = vec![];
    for af in [AF_INET, AF_INET6] {
        let mut size = 0u32;
        unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, af as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) };
        if size == 0 {
            continue;
        }
        let mut buf = vec![0u64; (size as usize).div_ceil(8) + 8];
        size = (buf.len() * 8) as u32;
        if unsafe { GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, af as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) } != 0 {
            continue;
        }
        unsafe {
            if af == AF_INET {
                let t = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
                let rows: &[MIB_TCPROW_OWNER_PID] = std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize);
                pids.extend(rows.iter().filter(|r| u16::from_be(r.dwLocalPort as u16) == port).map(|r| r.dwOwningPid as i32));
            } else {
                let t = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
                let rows: &[MIB_TCP6ROW_OWNER_PID] = std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize);
                pids.extend(rows.iter().filter(|r| u16::from_be(r.dwLocalPort as u16) == port).map(|r| r.dwOwningPid as i32));
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_in_the_table() {
        let me = std::process::id() as i32;
        let b = bsd_info(me).expect("own process");
        assert!(b.start > 1_600_000_000, "{}", b.start);
        assert!(!b.name.to_ascii_lowercase().ends_with(".exe"), "{}", b.name);
        assert!(exe_path(me).is_some_and(|p| p.to_ascii_lowercase().ends_with(".exe")));
        assert!(command_line(me).is_some());
        let here = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        assert_eq!(cwd(me).map(|c| c.to_ascii_lowercase()), Some(here.trim_end_matches('\\').to_ascii_lowercase()));
        assert!(footprint(me).is_some_and(|m| m > 0));
        assert!(cpu_ns(me).is_some());
    }

    #[test]
    fn finds_a_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(listening_pids(port).contains(&(std::process::id() as i32)));
    }

    #[test]
    fn strips_exe() {
        assert_eq!(strip_exe("node.EXE"), "node");
        assert_eq!(strip_exe("System"), "System");
        assert_eq!(strip_exe(".exe"), "");
    }
}
