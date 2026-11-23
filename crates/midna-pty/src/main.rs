//! `midna-pty.exe`: see `midna_pty::keeper`.

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(midna_pty::keeper::run(&args));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("midna-pty is the Windows terminal keeper; it doesn't run here.");
    std::process::exit(2);
}
