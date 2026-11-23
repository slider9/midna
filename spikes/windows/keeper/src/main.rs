//! T164 spike: does a per-terminal keeper process (the design's `midna-pty.exe`) hold a ConPTY
//! after the process that started it exits, and can other processes take its pipes over?
//!
//! One binary, several roles (first argument):
//!
//! - `launch`  stands in for the midnad that opens a terminal: starts a `keeper`, waits until it
//!   answers `hello`, prints what it said and exits.
//! - `keeper`  creates the pipes (with the given buffer size), the pseudoconsole, the job and the
//!   shell, then serves `\\.\pipe\<name>`: `hello`, `attach` (DuplicateHandle of the input and
//!   output pipes and the shell's process into the caller), `resize`, `repaint <ms>`, `hangup`,
//!   `kill`.
//! - `client`  stands in for a midnad that attaches: `type`, `hold`, `repaint`, `flood`.
//! - `flood`, `paint`  run inside the shell: print flat out (timing every write), or fill the
//!   screen with coloured rows.
//! - `drive`   runs the scenarios and prints a JSON report.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::*;

const COLS: i16 = 120;
const ROWS: i16 = 30;

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn arg_or(args: &[String], name: &str, d: &str) -> String {
    arg(args, name).unwrap_or_else(|| d.to_string())
}

static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn log(msg: &str) {
    if let Some(f) = LOG.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{:.0} [{}] {msg}", now_ms(), std::process::id());
    }
}

// ---------------------------------------------------------------------------------------------
// ConPTY: the bundled conpty.dll or Windows' own (as midnad's pty_windows::conpty does)

struct Conpty {
    create: unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32,
    resize: unsafe extern "system" fn(HPCON, COORD) -> i32,
    close: unsafe extern "system" fn(HPCON),
}

fn load_conpty(which: &str) -> Conpty {
    if which == "system" {
        return Conpty { create: CreatePseudoConsole, resize: ResizePseudoConsole, close: ClosePseudoConsole };
    }
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
    let path = wide(which);
    unsafe {
        let lib = LoadLibraryExW(path.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH);
        assert!(!lib.is_null(), "can't load {which}");
        let create = GetProcAddress(lib, c"ConptyCreatePseudoConsole".as_ptr().cast()).unwrap();
        let resize = GetProcAddress(lib, c"ConptyResizePseudoConsole".as_ptr().cast()).unwrap();
        let close = GetProcAddress(lib, c"ConptyClosePseudoConsole".as_ptr().cast()).unwrap();
        Conpty { create: std::mem::transmute(create), resize: std::mem::transmute(resize), close: std::mem::transmute(close) }
    }
}

// ---------------------------------------------------------------------------------------------
// Processes

fn pid_alive(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
        CloseHandle(h);
        alive
    }
}

fn terminate(pid: u32) -> bool {
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 9) != 0;
        CloseHandle(h);
        ok
    }
}

/// Every process now running: (pid, parent pid, exe name).
fn processes() -> Vec<(u32, u32, String)> {
    let mut out = vec![];
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
    }
    out
}

fn children_of(pid: u32) -> Vec<Value> {
    processes().into_iter().filter(|p| p.1 == pid).map(|p| json!({"pid": p.0, "exe": p.2})).collect()
}

fn in_job() -> bool {
    let mut r = 0;
    unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut r) };
    r != 0
}

// ---------------------------------------------------------------------------------------------
// keeper

struct Keeper {
    conpty: Conpty,
    hpc: Mutex<Option<HPCON>>,
    input: HANDLE,
    output: HANDLE,
    process: HANDLE,
    job: HANDLE,
    shell_pid: u32,
    exit_code: Mutex<Option<u32>>,
    size: Mutex<(i16, i16)>,
}
unsafe impl Send for Keeper {}
unsafe impl Sync for Keeper {}

fn keeper(args: &[String]) {
    let name = arg(args, "--name").expect("--name");
    *LOG.lock().unwrap() = arg(args, "--log").map(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap());
    let which = arg_or(args, "--conpty", "system");
    let buf: u32 = arg_or(args, "--buf", "0").parse().unwrap();
    let shell = arg_or(args, "--shell", "cmd.exe /d");
    log(&format!("keeper up: conpty={which} buf={buf} in_job={}", in_job()));
    let conpty = load_conpty(&which);
    let k = unsafe {
        let (mut in_r, mut in_w, mut out_r, mut out_w): (HANDLE, HANDLE, HANDLE, HANDLE) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        assert!(CreatePipe(&mut in_r, &mut in_w, std::ptr::null(), 0) != 0);
        // The output pipe's size is what a handover leans on: nobody reads it in between.
        assert!(CreatePipe(&mut out_r, &mut out_w, std::ptr::null(), buf) != 0);
        let mut hpc: HPCON = 0;
        let hr = (conpty.create)(COORD { X: COLS, Y: ROWS }, in_r, out_w, 0, &mut hpc);
        assert!(hr >= 0, "CreatePseudoConsole {hr:#x}");
        CloseHandle(in_r);
        CloseHandle(out_w);
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32);

        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        let mut attrs = vec![0u8; size];
        let list = attrs.as_mut_ptr() as *mut core::ffi::c_void;
        assert!(InitializeProcThreadAttributeList(list, 1, 0, &mut size) != 0);
        assert!(
            UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, hpc as *const _, std::mem::size_of::<HPCON>(), std::ptr::null_mut(), std::ptr::null())
                != 0
        );
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        si.lpAttributeList = list;
        let mut cmdline = wide(&shell);
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
            std::ptr::null(),
            std::ptr::null(),
            &si.StartupInfo,
            &mut pi,
        );
        assert!(ok != 0, "CreateProcessW: {}", std::io::Error::last_os_error());
        DeleteProcThreadAttributeList(list);
        AssignProcessToJobObject(job, pi.hProcess);
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        Arc::new(Keeper {
            conpty,
            hpc: Mutex::new(Some(hpc)),
            input: in_w,
            output: out_r,
            process: pi.hProcess,
            job,
            shell_pid: pi.dwProcessId,
            exit_code: Mutex::new(None),
            size: Mutex::new((COLS, ROWS)),
        })
    };
    log(&format!("shell pid {} children of keeper: {}", k.shell_pid, Value::Array(children_of(std::process::id()))));

    // The shell's exit: collect the code, close the console, leave a little later.
    {
        let k = k.clone();
        std::thread::spawn(move || unsafe {
            WaitForSingleObject(k.process, INFINITE);
            let mut code = 0u32;
            GetExitCodeProcess(k.process, &mut code);
            *k.exit_code.lock().unwrap() = Some(code);
            log(&format!("shell exited {code}"));
            close_console(&k);
            std::thread::sleep(Duration::from_secs(3));
            log("keeper exits");
            std::process::exit(0);
        });
    }

    let pipe_name = wide(&format!(r"\\.\pipe\{name}"));
    let mut first = true;
    loop {
        let flags = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        first = false;
        let h = unsafe { CreateNamedPipeW(pipe_name.as_ptr(), flags, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, 1, 4096, 4096, 0, std::ptr::null()) };
        assert!(h != INVALID_HANDLE_VALUE, "CreateNamedPipeW: {}", std::io::Error::last_os_error());
        let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !ok {
            unsafe { CloseHandle(h) };
            continue;
        }
        let mut client_pid = 0u32;
        unsafe { GetNamedPipeClientProcessId(h, &mut client_pid) };
        log(&format!("client {client_pid} connected"));
        let file = unsafe { std::fs::File::from_raw_handle(h as _) };
        serve(&k, file, client_pid);
        log(&format!("client {client_pid} gone"));
    }
}

fn close_console(k: &Keeper) {
    if let Some(hpc) = k.hpc.lock().unwrap().take() {
        let t = Instant::now();
        unsafe { (k.conpty.close)(hpc) };
        log(&format!("ClosePseudoConsole took {:?}", t.elapsed()));
    }
}

/// The visible screen as the console host holds it: attach to the shell's console for a moment
/// and read it with the console API (characters, legacy attributes, cursor).
fn peek(shell_pid: u32) -> Value {
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    unsafe {
        let t0 = Instant::now();
        FreeConsole();
        let t_free = t0.elapsed().as_secs_f64() * 1000.0;
        if AttachConsole(shell_pid) == 0 {
            return json!({"error": format!("AttachConsole: {}", std::io::Error::last_os_error())});
        }
        let t_attach = t0.elapsed().as_secs_f64() * 1000.0;
        let name = wide("CONOUT$");
        let h = CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        let r = if h == INVALID_HANDLE_VALUE || GetConsoleScreenBufferInfo(h, &mut info) == 0 {
            json!({"error": format!("CONOUT$: {}", std::io::Error::last_os_error())})
        } else {
            let w = info.srWindow;
            let (cols, rows) = ((w.Right - w.Left + 1) as usize, (w.Bottom - w.Top + 1) as usize);
            let mut cells: Vec<CHAR_INFO> = vec![std::mem::zeroed(); cols * rows];
            let mut region = w;
            let ok = ReadConsoleOutputW(h, cells.as_mut_ptr(), COORD { X: cols as i16, Y: rows as i16 }, COORD { X: 0, Y: 0 }, &mut region);
            let text: Vec<String> = (0..rows).map(|y| cells[y * cols..(y + 1) * cols].iter().map(|c| char::from_u32(c.Char.UnicodeChar as u32).unwrap_or('?')).collect()).collect();
            let attrs: Vec<Vec<u16>> = (0..rows).map(|y| cells[y * cols..(y + 1) * cols].iter().map(|c| c.Attributes).collect()).collect();
            json!({
                "ok": ok != 0, "cols": cols, "rows": rows,
                "buffer": [info.dwSize.X, info.dwSize.Y], "window_top": w.Top,
                "cursor": [info.dwCursorPosition.Y - w.Top, info.dwCursorPosition.X],
                "text": text, "attrs": attrs,
            })
        };
        let t_read = t0.elapsed().as_secs_f64() * 1000.0;
        if h != INVALID_HANDLE_VALUE {
            CloseHandle(h);
        }
        FreeConsole();
        let mut r = r;
        r["ms"] = json!({"free": t_free, "attach": t_attach, "read": t_read, "detach": t0.elapsed().as_secs_f64() * 1000.0});
        r
    }
}

/// A peek turned back into VT, for a fresh screen: what a new midnad would feed its engine.
fn peek_to_vt(p: &Value) -> Vec<u8> {
    let mut vt = String::from("\x1b[H\x1b[2J");
    let text = p["text"].as_array().cloned().unwrap_or_default();
    let attrs = p["attrs"].as_array().cloned().unwrap_or_default();
    for (y, row) in text.iter().enumerate() {
        vt.push_str(&format!("\x1b[{};1H", y + 1));
        let mut prev = u16::MAX;
        for (x, ch) in row.as_str().unwrap().chars().enumerate() {
            let a = attrs[y][x].as_u64().unwrap() as u16;
            if a & 0x200 != 0 {
                continue; // the second half of a wide character
            }
            if a != prev {
                let idx = |v: u16| (if v & 4 != 0 { 1 } else { 0 }) | (if v & 2 != 0 { 2 } else { 0 }) | (if v & 1 != 0 { 4 } else { 0 });
                let (f, b) = (a & 0xf, (a >> 4) & 0xf);
                let fg = if f == 7 { 39 } else if f & 8 != 0 { 90 + idx(f) } else { 30 + idx(f) };
                let bg = if b == 0 { 49 } else if b & 8 != 0 { 100 + idx(b) } else { 40 + idx(b) };
                vt.push_str(&format!("\x1b[0;{fg};{bg}{}{}m", if a & 0x8000 != 0 { ";4" } else { "" }, if a & 0x4000 != 0 { ";7" } else { "" }));
                prev = a;
            }
            vt.push(ch);
        }
    }
    let c = &p["cursor"];
    vt.push_str(&format!("\x1b[0m\x1b[{};{}H", c[0].as_i64().unwrap_or(0) + 1, c[1].as_i64().unwrap_or(0) + 1));
    vt.into_bytes()
}

fn dup_into(target: HANDLE, h: HANDLE) -> usize {
    let mut out: HANDLE = std::ptr::null_mut();
    let ok = unsafe { DuplicateHandle(GetCurrentProcess(), h, target, &mut out, 0, 0, DUPLICATE_SAME_ACCESS) };
    assert!(ok != 0, "DuplicateHandle: {}", std::io::Error::last_os_error());
    out as usize
}

fn serve(k: &Arc<Keeper>, file: std::fs::File, client_pid: u32) {
    let mut w = file.try_clone().unwrap();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let parts: Vec<&str> = line.split_whitespace().collect();
        let t = Instant::now();
        let reply = match parts.as_slice() {
            ["hello"] => json!({
                "keeper_pid": std::process::id(),
                "shell_pid": k.shell_pid,
                "shell_alive": pid_alive(k.shell_pid),
                "exit_code": *k.exit_code.lock().unwrap(),
                "keeper_in_job": in_job(),
                "keeper_children": children_of(std::process::id()),
            }),
            ["attach"] => unsafe {
                // The caller's pid comes from the pipe itself, not from what it claims.
                let target = OpenProcess(PROCESS_DUP_HANDLE, 0, client_pid);
                if target.is_null() {
                    json!({"error": format!("OpenProcess({client_pid}): {}", std::io::Error::last_os_error())})
                } else {
                    let r = json!({
                        "input": dup_into(target, k.input),
                        "output": dup_into(target, k.output),
                        "process": dup_into(target, k.process),
                        "shell_pid": k.shell_pid,
                    });
                    CloseHandle(target);
                    r
                }
            },
            ["resize", c, r] => {
                let (c, r): (i16, i16) = (c.parse().unwrap(), r.parse().unwrap());
                *k.size.lock().unwrap() = (c, r);
                let hr = k.hpc.lock().unwrap().map(|h| unsafe { (k.conpty.resize)(h, COORD { X: c, Y: r }) });
                json!({"hr": hr})
            }
            ["repaint", ms, dc, dr] => {
                // The nudge: `dc` columns and `dr` rows smaller, then back (the design: 1 column).
                let (c, r) = *k.size.lock().unwrap();
                let ms: u64 = ms.parse().unwrap();
                let (dc, dr): (i16, i16) = (dc.parse().unwrap(), dr.parse().unwrap());
                let hpc = *k.hpc.lock().unwrap();
                let (a, b) = match hpc {
                    Some(h) => unsafe {
                        let a = (k.conpty.resize)(h, COORD { X: c - dc, Y: r - dr });
                        std::thread::sleep(Duration::from_millis(ms));
                        let b = (k.conpty.resize)(h, COORD { X: c, Y: r });
                        (a, b)
                    },
                    None => (-1, -1),
                };
                json!({"hr": [a, b]})
            }
            ["peek"] => peek(k.shell_pid),
            ["hangup"] => {
                close_console(k);
                json!({"ok": true})
            }
            ["hangup-drain"] => {
                // With no midnad reading, read the output pipe ourselves (and drop it), so the
                // console host can finish writing and act on the close.
                let out = k.output as usize;
                let drained = Arc::new(AtomicU64::new(0));
                let d = drained.clone();
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; 65536];
                    loop {
                        let mut n = 0u32;
                        if unsafe { ReadFile(out as HANDLE, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) } == 0 || n == 0 {
                            break;
                        }
                        d.fetch_add(n as u64, Ordering::SeqCst);
                    }
                });
                close_console(k);
                let t = Instant::now();
                let gone = unsafe { WaitForSingleObject(k.process, 5000) } == WAIT_OBJECT_0;
                json!({"ok": true, "shell_ended": gone, "shell_end_ms": t.elapsed().as_secs_f64() * 1000.0, "drained_bytes": drained.load(Ordering::SeqCst)})
            }
            ["kill"] => {
                unsafe { TerminateJobObject(k.job, 1) };
                json!({"ok": true})
            }
            _ => json!({"error": format!("unknown: {line}")}),
        };
        let mut reply = reply;
        reply["took_ms"] = json!(t.elapsed().as_secs_f64() * 1000.0);
        log(&format!("{line} -> {reply}"));
        if writeln!(w, "{reply}").is_err() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Control-pipe client

struct Ctl {
    w: std::fs::File,
    r: BufReader<std::fs::File>,
}

impl Ctl {
    fn connect(name: &str) -> std::io::Result<Ctl> {
        let path = format!(r"\\.\pipe\{name}");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
                Ok(f) => return Ok(Ctl { w: f.try_clone()?, r: BufReader::new(f) }),
                Err(e) if Instant::now() > deadline => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
    fn call(&mut self, cmd: &str) -> Value {
        writeln!(self.w, "{cmd}").unwrap();
        let mut line = String::new();
        self.r.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or(json!({"error": "no reply", "raw": line}))
    }
}

// ---------------------------------------------------------------------------------------------
// launch

fn launch(args: &[String]) {
    let exe = std::env::current_exe().unwrap();
    let mut keeper_args = vec!["keeper".to_string()];
    keeper_args.extend(args.iter().cloned());
    // CreateProcessW with no handle inheritance: std's Command inherits every inheritable handle,
    // so the keeper would hold the launcher's stdout pipe (and whatever else midnad has) forever.
    let mut line = String::new();
    for a in std::iter::once(exe.display().to_string()).chain(keeper_args) {
        line.push_str(&format!("\"{a}\" "));
    }
    let spawn = |flags: u32| -> Result<u32, std::io::Error> {
        let mut cl = wide(&line);
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { CreateProcessW(std::ptr::null(), cl.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, flags, std::ptr::null(), std::ptr::null(), &si, &mut pi) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        unsafe {
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
        }
        Ok(pi.dwProcessId)
    };
    // As the design says: no window, and out of any job the launcher (midnad) runs in.
    // --detached: no console of its own at all (it runs no console programs but the shell, which
    // has the pseudoconsole), so no extra conhost.exe per terminal.
    let base = if args.iter().any(|a| a == "--detached") { DETACHED_PROCESS } else { CREATE_NO_WINDOW };
    let mut breakaway = true;
    let child_pid = match spawn(base | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(p) => p,
        Err(e) => {
            breakaway = false;
            eprintln!("breakaway refused ({e}); starting the keeper inside the job");
            spawn(base | CREATE_NEW_PROCESS_GROUP).unwrap()
        }
    };
    let name = arg(args, "--name").unwrap();
    let mut ctl = Ctl::connect(&name).expect("keeper never served its pipe");
    let mut hello = ctl.call("hello");
    hello["launcher_pid"] = json!(std::process::id());
    hello["launcher_in_job"] = json!(in_job());
    hello["breakaway"] = json!(breakaway);
    hello["spawned_keeper_pid"] = json!(child_pid);
    println!("{hello}");
    // Exit without waiting on the keeper: it has to stand on its own now.
}

// ---------------------------------------------------------------------------------------------
// client

struct Attached {
    input: HANDLE,
    output: HANDLE,
    process: HANDLE,
    shell_pid: u32,
}
unsafe impl Send for Attached {}
unsafe impl Sync for Attached {}

fn attach(ctl: &mut Ctl) -> Attached {
    let r = ctl.call("attach");
    assert!(r.get("error").is_none(), "{r}");
    let h = |k: &str| r[k].as_u64().unwrap() as usize as HANDLE;
    Attached { input: h("input"), output: h("output"), process: h("process"), shell_pid: r["shell_pid"].as_u64().unwrap() as u32 }
}

fn write_all(h: HANDLE, mut b: &[u8]) {
    while !b.is_empty() {
        let mut n = 0u32;
        let ok = unsafe { WriteFile(h, b.as_ptr(), b.len() as u32, &mut n, std::ptr::null_mut()) };
        assert!(ok != 0, "WriteFile: {}", std::io::Error::last_os_error());
        b = &b[n as usize..];
    }
}

/// Reads the output pipe on a thread; `paused` stops it between reads.
fn reader(output: HANDLE, paused: Arc<AtomicBool>) -> mpsc::Receiver<(f64, Vec<u8>)> {
    let (tx, rx) = mpsc::channel();
    let h = output as usize;
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 65536];
        loop {
            while paused.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut n = 0u32;
            let ok = unsafe { ReadFile(h as HANDLE, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) };
            if ok == 0 || n == 0 {
                break;
            }
            if tx.send((now_ms(), buf[..n as usize].to_vec())).is_err() {
                break;
            }
        }
    });
    rx
}

/// Text with escape sequences taken out, for finding a marker.
fn strip(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.next() {
                Some('[') => {
                    while let Some(c) = it.next() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = it.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c >= ' ' {
            out.push(c);
        }
    }
    out
}

fn append(path: &str, b: &[u8]) {
    std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap().write_all(b).unwrap();
}

fn client(args: &[String]) {
    let mode = args[0].as_str();
    let name = arg(args, "--name").unwrap();
    let capture = arg(args, "--capture");
    let mut ctl = Ctl::connect(&name).unwrap();
    let a = attach(&mut ctl);
    let paused = Arc::new(AtomicBool::new(false));
    let mut result = json!({"client_pid": std::process::id(), "shell_pid": a.shell_pid,
        "shell_running_via_dup_handle": unsafe { WaitForSingleObject(a.process, 0) } == WAIT_TIMEOUT});
    match mode {
        "type" => {
            let rx = reader(a.output, paused.clone());
            let text = arg(args, "--text").unwrap();
            let expect = arg(args, "--expect");
            write_all(a.input, format!("{text}\r").as_bytes());
            let mut got = vec![];
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut seen = expect.is_none();
            while Instant::now() < deadline {
                if let Ok((_, b)) = rx.recv_timeout(Duration::from_millis(300)) {
                    got.extend(b);
                    if let Some(e) = &expect
                        && strip(&got).contains(e.as_str())
                    {
                        seen = true;
                    }
                } else if seen {
                    break; // quiet for 300 ms after the marker: the prompt is back
                }
            }
            if let Some(c) = &capture {
                append(c, &got);
            }
            result["seen"] = json!(seen);
            result["bytes"] = json!(got.len());
        }
        "hold" => {
            // A midnad that has the handles and is about to be killed: never reads.
            println!("{result}");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(600));
        }
        "repaint" => {
            let rx = reader(a.output, paused.clone());
            let mut pre = vec![];
            while let Ok((_, b)) = rx.recv_timeout(Duration::from_millis(700)) {
                pre.extend(b);
            }
            let delay = arg_or(args, "--delay", "0");
            let nudge = arg_or(args, "--nudge", "1 0");
            result["repaint"] = ctl.call(&format!("repaint {delay} {nudge}"));
            let mut post = vec![];
            let deadline = Instant::now() + Duration::from_millis(2000);
            while Instant::now() < deadline {
                if let Ok((_, b)) = rx.recv_timeout(Duration::from_millis(100)) {
                    post.extend(b);
                }
            }
            append(&arg(args, "--pre").unwrap(), &pre);
            std::fs::write(arg(args, "--post").unwrap(), &post).unwrap();
            result["pre_bytes"] = json!(pre.len());
            result["post_bytes"] = json!(post.len());
        }
        "flood" => {
            // Read as a midnad would, but stop for `--pause` ms (the handover) partway.
            let rx = reader(a.output, paused.clone());
            let stats = arg(args, "--stats").unwrap();
            let _ = std::fs::remove_file(&stats);
            let exe = std::env::current_exe().unwrap();
            let secs = arg_or(args, "--secs", "6");
            write_all(a.input, format!("\"{}\" flood --secs {secs} --stats \"{stats}\"\r", exe.display()).as_bytes());
            let pause_at: f64 = arg_or(args, "--pause-at", "2500").parse().unwrap();
            let pause_for: f64 = arg_or(args, "--pause", "1000").parse().unwrap();
            let start = now_ms();
            let total = Arc::new(AtomicU64::new(0));
            let mut buckets: Vec<u64> = vec![];
            let (mut p0, mut p1) = (0.0, 0.0);
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                let t = now_ms() - start;
                if p0 == 0.0 && t >= pause_at {
                    p0 = now_ms();
                    paused.store(true, Ordering::SeqCst);
                }
                if p0 != 0.0 && p1 == 0.0 && now_ms() - p0 >= pause_for {
                    p1 = now_ms();
                    paused.store(false, Ordering::SeqCst);
                }
                if let Ok((at, b)) = rx.recv_timeout(Duration::from_millis(5)) {
                    let i = ((at - start) / 100.0) as usize;
                    if buckets.len() <= i {
                        buckets.resize(i + 1, 0);
                    }
                    buckets[i] += b.len() as u64;
                    total.fetch_add(b.len() as u64, Ordering::SeqCst);
                }
                if p1 != 0.0 && Path::new(&stats).exists() {
                    std::thread::sleep(Duration::from_millis(300));
                    while let Ok(_) = rx.try_recv() {}
                    break;
                }
            }
            result["pause_start_ms"] = json!(p0);
            result["pause_end_ms"] = json!(p1);
            result["bytes"] = json!(total.load(Ordering::SeqCst));
            result["bytes_per_100ms"] = json!(buckets);
        }
        "exit" => {
            // Ends the shell and collects its code through the duplicated process handle.
            write_all(a.input, b"exit 7\r");
            let w = unsafe { WaitForSingleObject(a.process, 10_000) };
            let mut code = 0u32;
            unsafe { GetExitCodeProcess(a.process, &mut code) };
            result["wait"] = json!(w == WAIT_OBJECT_0);
            result["exit_code"] = json!(code);
        }
        _ => panic!("client mode {mode}"),
    }
    println!("{result}");
}

// ---------------------------------------------------------------------------------------------
// In the shell

fn flood(args: &[String]) {
    let secs: f64 = arg_or(args, "--secs", "6").parse().unwrap();
    let stats = arg(args, "--stats").unwrap();
    let out = std::io::stdout();
    let mut lock = out.lock();
    let start = Instant::now();
    let (mut n, mut max, mut max_at) = (0u64, 0f64, 0f64);
    let mut stalls = vec![];
    let pad = "y".repeat(100);
    while start.elapsed().as_secs_f64() < secs {
        let wall = now_ms();
        let t = Instant::now();
        let _ = writeln!(lock, "{n:08} {pad}");
        let _ = lock.flush();
        let dt = t.elapsed().as_secs_f64() * 1000.0;
        if dt > max {
            max = dt;
            max_at = wall;
        }
        if dt > 50.0 {
            stalls.push(json!({"at": wall, "ms": dt}));
        }
        n += 1;
    }
    let s = json!({"lines": n, "secs": secs, "max_write_ms": max, "max_at": max_at, "stalls_over_50ms": stalls});
    std::fs::write(stats, s.to_string()).unwrap();
}

fn paint(_args: &[String]) {
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut m = 0;
        GetConsoleMode(h, &mut m);
        SetConsoleMode(h, m | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    }
    let mut o = std::io::stdout().lock();
    for i in 0..45 {
        let fg = 31 + (i % 7);
        let bg = 40 + ((i + 3) % 8);
        let _ = writeln!(o, "\x1b[{fg}mrow {i:02} \x1b[1;{bg}m bold-on-bg \x1b[0m plain \x1b[4munderlined\x1b[0m \x1b[7minverse {i}\x1b[0m end");
    }
    let _ = writeln!(o, "PAINT-END");
}

// ---------------------------------------------------------------------------------------------
// drive

struct Run {
    exe: PathBuf,
    dir: PathBuf,
    name: String,
    conpty: String,
    detached: bool,
}

impl Run {
    fn new(dir: &Path, conpty: &str, tag: &str) -> Run {
        let name = format!("keeper-spike-{}-{tag}", std::process::id());
        Run { exe: std::env::current_exe().unwrap(), dir: dir.to_path_buf(), name, conpty: conpty.to_string(), detached: std::env::var_os("SPIKE_DETACHED").is_some() }
    }
    fn file(&self, f: &str) -> String {
        self.dir.join(format!("{}-{f}", self.name)).display().to_string()
    }
    fn launch(&self, buf: u32) -> Value {
        let out = Command::new(&self.exe)
            .args(["launch", "--name", &self.name, "--conpty", &self.conpty, "--buf", &buf.to_string(), "--log", &self.file("keeper.log")])
            .args(if self.detached { &["--detached"][..] } else { &[][..] })
            .output()
            .unwrap();
        let mut v: Value = serde_json::from_slice(&out.stdout).unwrap_or(json!({"error": String::from_utf8_lossy(&out.stderr)}));
        v["launcher_exit"] = json!(out.status.code());
        v
    }
    fn client(&self, args: &[&str]) -> Value {
        let out = Command::new(&self.exe).arg("client").args(args).args(["--name", &self.name]).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or(json!({"error": String::from_utf8_lossy(&out.stderr), "status": out.status.code()}))
    }
    fn ctl(&self, cmd: &str) -> Value {
        Ctl::connect(&self.name).map(|mut c| c.call(cmd)).unwrap_or(json!({"error": "no keeper"}))
    }
}

/// Screen text, one string per row, plus the cursor, and each cell's look.
fn screen(bytes: &[u8]) -> (Vec<String>, (u16, u16), Vec<String>) {
    let mut p = vtparser();
    p.process(bytes);
    let s = p.screen();
    let rows: Vec<String> = s.rows(0, COLS as u16).map(|r| r.trim_end().to_string()).collect();
    let mut looks = vec![];
    for r in 0..ROWS as u16 {
        let mut line = String::new();
        for c in 0..COLS as u16 {
            if let Some(cell) = s.cell(r, c) {
                line.push_str(&format!("{:?}{:?}{}{}{}|", cell.fgcolor(), cell.bgcolor(), cell.bold() as u8, cell.underline() as u8, cell.inverse() as u8));
            }
        }
        looks.push(line);
    }
    (rows, s.cursor_position(), looks)
}

fn vtparser() -> vt100::Parser {
    vt100::Parser::new(ROWS as u16, COLS as u16, 0)
}

fn compare(truth: &[u8], fresh: &[u8]) -> Value {
    let (tr, tc, tl) = screen(truth);
    let (fr, fc, fl) = screen(fresh);
    let text_rows_differ: Vec<Value> =
        (0..tr.len()).filter(|&i| tr[i] != fr[i]).map(|i| json!({"row": i, "truth": tr[i], "fresh": fr[i]})).collect();
    let look_rows_differ: Vec<usize> = (0..tl.len()).filter(|&i| tl[i] != fl[i]).collect();
    json!({
        "text_rows_differ": text_rows_differ,
        "style_rows_differ": look_rows_differ,
        "cursor_truth": [tc.0, tc.1],
        "cursor_fresh": [fc.0, fc.1],
        "fresh_nonblank_rows": fr.iter().filter(|r| !r.trim().is_empty()).count(),
        "truth_nonblank_rows": tr.iter().filter(|r| !r.trim().is_empty()).count(),
        "truth_screen": tr,
    })
}

/// Q1, Q4 and the handle passing: launcher exits; client A types; client B holds the handles and
/// is killed; client C types; client D repaints; then the keeper is killed.
fn drive_basic(dir: &Path, conpty: &str, tag: &str) -> Value {
    let run = Run::new(dir, conpty, tag);
    let mut r = json!({"conpty": conpty});
    let launched = run.launch(0);
    let launcher_pid = launched["launcher_pid"].as_u64().unwrap_or(0) as u32;
    let keeper_pid = launched["keeper_pid"].as_u64().unwrap_or(0) as u32;
    let shell_pid = launched["shell_pid"].as_u64().unwrap_or(0) as u32;
    r["launch"] = launched;
    std::thread::sleep(Duration::from_millis(1500));
    r["after_launcher_exit"] = json!({
        "launcher_alive": pid_alive(launcher_pid),
        "keeper_alive": pid_alive(keeper_pid),
        "shell_alive": pid_alive(shell_pid),
        "keeper_children": children_of(keeper_pid),
    });
    let truth = run.file("truth.bin");
    r["client_a"] = run.client(&["type", "--text", "echo MARK-A-%ERRORLEVEL%Z", "--expect", "MARK-A-0Z", "--capture", &truth]);
    // B: a midnad killed while it holds the handles (taskkill /F).
    let mut b = Command::new(&run.exe).args(["client", "hold", "--name", &run.name]).stdout(Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    BufReader::new(b.stdout.take().unwrap()).read_line(&mut line).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let killed = terminate(b.id());
    let _ = b.wait();
    std::thread::sleep(Duration::from_millis(500));
    r["client_b_killed"] = json!({"hold": serde_json::from_str::<Value>(&line).unwrap_or(json!(line)), "killed": killed, "shell_alive_after": pid_alive(shell_pid)});
    r["client_c"] = run.client(&["type", "--text", "echo MARK-C-%ERRORLEVEL%Z", "--expect", "MARK-C-0Z", "--capture", &truth]);
    let paint = format!("\"{}\" paint", run.exe.display());
    r["client_paint"] = run.client(&["type", "--text", &paint, "--expect", "PAINT-END", "--capture", &truth]);
    // The other way to get the screen back: read it from the console host with the console API.
    let truth_b = std::fs::read(&truth).unwrap();
    let t = Instant::now();
    let p = run.ctl("peek");
    let took = t.elapsed().as_secs_f64() * 1000.0;
    let vt = peek_to_vt(&p);
    r["peek"] = json!({"took_ms": took, "steps_ms": p.get("ms"), "error": p.get("error"), "cursor": p.get("cursor"), "buffer": p.get("buffer"), "window_top": p.get("window_top"), "compare": compare(&truth_b, &vt)});
    r["after_peek_shell_alive"] = json!(pid_alive(shell_pid));
    r["client_after_peek"] = run.client(&["type", "--text", "echo MARK-P-%ERRORLEVEL%Z", "--expect", "MARK-P-0Z"]);
    // D: a midnad that comes up after a crash, with nothing of the screen: drain, nudge, read.
    for (i, (delay, nudge)) in [("0", "1 0"), ("100", "1 0"), ("500", "1 0"), ("100", "0 1"), ("100", "10 5")].iter().enumerate() {
        let post = run.file(&format!("post{i}.bin"));
        let d = run.client(&["repaint", "--delay", delay, "--nudge", nudge, "--pre", &truth, "--post", &post]);
        let post_b = std::fs::read(&post).unwrap_or_default();
        append(&truth, &post_b);
        let truth_b = std::fs::read(&truth).unwrap();
        r[format!("repaint_{delay}ms_{}", nudge.replace(' ', "x"))] = json!({"client": d, "compare": compare(&truth_b, &post_b)});
    }
    r["hello_end"] = run.ctl("hello");
    r["client_exit"] = run.client(&["exit"]);
    std::thread::sleep(Duration::from_millis(500));
    r["keeper_alive_after_shell_exit_500ms"] = json!(pid_alive(keeper_pid));
    std::thread::sleep(Duration::from_millis(3500));
    r["keeper_alive_after_shell_exit_4s"] = json!(pid_alive(keeper_pid));
    if pid_alive(keeper_pid) {
        let _ = run.ctl("kill");
        terminate(keeper_pid);
    }
    // A second keeper, killed while its shell runs: a keeper that crashes takes its terminal with it.
    let run2 = Run::new(dir, conpty, &format!("{tag}k"));
    let l2 = run2.launch(0);
    let (keeper_pid, shell_pid) = (l2["keeper_pid"].as_u64().unwrap_or(0) as u32, l2["shell_pid"].as_u64().unwrap_or(0) as u32);
    std::thread::sleep(Duration::from_millis(1000));
    let hosts = children_of(keeper_pid);
    let killed = terminate(keeper_pid);
    std::thread::sleep(Duration::from_millis(2000));
    let host = hosts;
    r["keeper_killed"] = json!({
        "killed": killed,
        "shell_alive_after": pid_alive(shell_pid),
        "hosts_alive_after": host.iter().map(|h| json!({"exe": h["exe"], "alive": pid_alive(h["pid"].as_u64().unwrap() as u32)})).collect::<Vec<_>>(),
    });
    r
}

/// Q3: a program printing flat out while the reader stops for `pause` ms.
fn drive_flood(dir: &Path, conpty: &str, buf: u32, pause: u32, tag: &str) -> Value {
    let run = Run::new(dir, conpty, tag);
    let launched = run.launch(buf);
    let keeper_pid = launched["keeper_pid"].as_u64().unwrap_or(0) as u32;
    let stats = run.file("flood.json");
    let c = run.client(&["flood", "--stats", &stats, "--secs", "6", "--pause-at", "2500", "--pause", &pause.to_string()]);
    let s: Value = std::fs::read(&stats).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!(null));
    let (p0, p1) = (c["pause_start_ms"].as_f64().unwrap_or(0.0), c["pause_end_ms"].as_f64().unwrap_or(0.0));
    // Writes that were stuck during the pause: how long the program was blocked by it.
    let blocked: f64 = s["stalls_over_50ms"]
        .as_array()
        .map(|a| a.iter().filter(|x| x["at"].as_f64().unwrap() < p1 + 50.0 && x["at"].as_f64().unwrap() + x["ms"].as_f64().unwrap() > p0).map(|x| x["ms"].as_f64().unwrap()).sum())
        .unwrap_or(0.0);
    let buckets = c["bytes_per_100ms"].as_array().cloned().unwrap_or_default();
    let reading: Vec<u64> = buckets.iter().take(24).skip(5).map(|v| v.as_u64().unwrap()).collect();
    let rate = if reading.is_empty() { 0 } else { reading.iter().sum::<u64>() * 10 / reading.len() as u64 };
    let first_after: u64 = buckets.iter().skip(((p1 - (p0 - 2500.0)) / 100.0).max(0.0) as usize).take(2).map(|v| v.as_u64().unwrap()).sum();
    let _ = run.ctl("kill");
    let _ = keeper_pid;
    json!({
        "conpty": conpty, "buf": buf, "pause_ms": pause,
        "lines_per_s": s["lines"].as_f64().map(|l| l / 6.0),
        "max_write_ms": s["max_write_ms"],
        "blocked_during_pause_ms": blocked,
        "stalls": s["stalls_over_50ms"],
        "read_bytes_per_s_while_reading": rate,
        "bytes_in_first_200ms_after_pause": first_after,
        "total_bytes": c["bytes"],
        "buckets": buckets,
    })
}

/// Hang-up while nobody reads a full output pipe (a midnad crashed mid-flood, then a stop).
fn drive_hangup(dir: &Path, conpty: &str, tag: &str, how: &str) -> Value {
    let run = Run::new(dir, conpty, tag);
    let launched = run.launch(0);
    let shell_pid = launched["shell_pid"].as_u64().unwrap_or(0) as u32;
    let keeper_pid = launched["keeper_pid"].as_u64().unwrap_or(0) as u32;
    let stats = run.file("hangup-flood.json");
    let flood = format!("\"{}\" flood --secs 20 --stats \"{stats}\"", run.exe.display());
    // Type it and leave without reading anything more.
    let c = run.client(&["type", "--text", &flood]);
    std::thread::sleep(Duration::from_millis(1500));
    let t = Instant::now();
    let name = run.name.clone();
    let (tx, rx) = mpsc::channel();
    let how2 = how.to_string();
    std::thread::spawn(move || {
        let v = Ctl::connect(&name).map(|mut c| c.call(&how2)).unwrap_or(json!({"error": "no keeper"}));
        let _ = tx.send(v);
    });
    let reply = rx.recv_timeout(Duration::from_secs(15)).ok();
    let took = t.elapsed().as_secs_f64() * 1000.0;
    std::thread::sleep(Duration::from_millis(1000));
    let r = json!({
        "conpty": conpty, "client": c,
        "hangup_reply": reply, "hangup_returned_ms": took,
        "shell_alive_after": pid_alive(shell_pid),
        "keeper_alive_after": pid_alive(keeper_pid),
    });
    if pid_alive(keeper_pid) {
        let _ = run.ctl("kill");
        std::thread::sleep(Duration::from_millis(500));
        terminate(keeper_pid);
    }
    r
}

fn drive(args: &[String]) {
    let dir = PathBuf::from(arg(args, "--dir").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let conpties: Vec<String> = arg(args, "--conpty").unwrap().split(';').map(String::from).collect();
    let what = arg_or(args, "--what", "basic,flood,hangup");
    let bufs: Vec<u32> = arg_or(args, "--bufs", "0,65536,1048576,8388608").split(',').map(|s| s.parse().unwrap()).collect();
    let pauses: Vec<u32> = arg_or(args, "--pauses", "1000").split(',').map(|s| s.parse().unwrap()).collect();
    let mut report = json!({"driver_in_job": in_job()});
    for (ci, cp) in conpties.iter().enumerate() {
        let key = if cp == "system" { "system".to_string() } else { "bundled".to_string() };
        let mut part = json!({});
        if what.contains("basic") {
            part["basic"] = drive_basic(&dir, cp, &format!("{ci}b"));
            eprintln!("{key} basic done");
        }
        if what.contains("flood") {
            let mut v = vec![];
            for &b in &bufs {
                for &p in &pauses {
                    v.push(drive_flood(&dir, cp, b, p, &format!("{ci}f{b}p{p}")));
                    eprintln!("{key} flood buf={b} pause={p} done");
                }
            }
            part["flood"] = json!(v);
        }
        if what.contains("hangup") {
            part["hangup"] = drive_hangup(&dir, cp, &format!("{ci}h"), "hangup");
            part["hangup_drain"] = drive_hangup(&dir, cp, &format!("{ci}d"), "hangup-drain");
            eprintln!("{key} hangup done");
        }
        report[key] = part;
    }
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = &args[1.min(args.len())..];
    match args.first().map(String::as_str) {
        Some("keeper") => keeper(rest),
        Some("launch") => launch(rest),
        Some("client") => client(rest),
        Some("flood") => flood(rest),
        Some("paint") => paint(rest),
        Some("drive") => drive(rest),
        _ => {
            let _ = std::io::stdin().read(&mut [0u8; 0]);
            eprintln!("usage: keeper-spike drive --dir DIR --conpty 'PATH;system' [--what basic,flood,hangup] [--bufs ..] [--pauses ..]");
            std::process::exit(2);
        }
    }
}
