//! Windows: starting a program that is a batch file (`.cmd`, `.bat`) without cmd.exe.
//!
//! A batch file only runs through cmd.exe, and cmd reads its arguments as batch text: it ends
//! them at the first newline and acts on `|`, `<`, `>` and `&` in them. No quoting gets a newline
//! past it, and an agent's arguments carry both (Claude's system prompt, Codex's `notify=[…]`).
//! Most batch files that start an agent are shims that only pass `%*` on to a real program:
//! npm's `claude.cmd` and `codex.cmd` run `node.exe` on the package's script. [`resolve`] reads
//! such a shim and returns the command it would run, so midna starts that program itself with
//! the arguments quoted for it. A batch file that does anything else is left as it is, and only
//! then does it go through cmd.
use std::path::{Path, PathBuf};

/// `argv` as a program to start: its first word found on `path` (`;`-separated, with PATHEXT),
/// and a batch-file shim there replaced by what it runs, its arguments in front of the rest of
/// `argv`. None when the program isn't found. The program in the result is a batch file only when
/// it isn't a shim [`target`] can read; that one still needs cmd.
pub fn resolve(argv: &[String], path: &str) -> Option<Vec<String>> {
    let (prog, rest) = argv.split_first()?;
    let mut prefix = vec![which(prog, path)?];
    // A shim may name another shim (npx's, a version manager's); three is plenty.
    for _ in 0..3 {
        if !is_batch(&prefix[0]) {
            break;
        }
        let Some(mut inner) = target(Path::new(&prefix[0])) else { break };
        let Some(exe) = which(&inner[0], path) else { break };
        inner[0] = exe;
        inner.extend(prefix.drain(1..));
        prefix = inner;
    }
    prefix.extend(rest.iter().cloned());
    Some(prefix)
}

/// A `.cmd` or `.bat` file, which CreateProcess can only run through cmd.exe.
pub fn is_batch(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    l.ends_with(".cmd") || l.ends_with(".bat")
}

/// The command the batch file `file` runs with its arguments after it (`%*`): the program as it
/// names it (a path, or a name to look up on PATH) and its own arguments. None unless `file` is
/// a shim that does nothing else: one command line ending in `%*`, after nothing but npm's
/// cmd-shim preamble or `@echo off`.
pub fn target(file: &Path) -> Option<Vec<String>> {
    let text = std::fs::read(file).ok().filter(|b| b.len() < 16 * 1024)?;
    let dir = file.parent()?.to_string_lossy().trim_end_matches('\\').to_string() + "\\";
    parse(&String::from_utf8_lossy(&text), &dir, &|p| Path::new(p).is_file())
}

/// Lines npm's cmd-shim writes before its command (and `@echo off`), compared trimmed and
/// ignoring case. `IF EXIST`/`SET "_prog=` are read separately.
const PREAMBLE: &[&str] = &[
    "@echo off",
    "goto start",
    ":find_dp0",
    "set dp0=%~dp0",
    "exit /b",
    ":start",
    "setlocal",
    "@setlocal",
    "call :find_dp0",
    ") else (",
    ")",
    "set pathext=%pathext:;.js;=;%",
];

/// What npm's shim writes before its command so Ctrl+C doesn't ask "Terminate batch job?".
const NPM_PREFIX: &str = "endlocal & goto #_undefined_# 2>nul || title %comspec% & ";

/// [`target`] on a batch file's text, `dir` its folder with a trailing `\`, `exists` whether a
/// file is there (for npm's `IF EXIST "%dp0%\node.exe"`).
fn parse(text: &str, dir: &str, exists: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let (last, before) = lines.split_last()?;
    // npm: IF EXIST "%dp0%\node.exe" ( SET "_prog=%dp0%\node.exe" ) ELSE ( SET "_prog=node" … )
    let (mut if_exists, mut progs) = (None, vec![]);
    for l in before {
        let lower = l.to_ascii_lowercase();
        if PREAMBLE.contains(&lower.as_str()) {
        } else if let Some(p) = lower.strip_prefix("if exist ").and_then(|r| r.strip_suffix(" (")) {
            if if_exists.is_some() {
                return None;
            }
            if_exists = Some(l[9..9 + p.len()].trim_matches('"').to_string());
        } else if lower.starts_with("set \"_prog=") && lower.ends_with('"') {
            progs.push(l[11..l.len() - 1].to_string());
        } else {
            return None;
        }
    }
    let prog = match (if_exists, progs.as_slice()) {
        (None, []) => None,
        (Some(cond), [then, otherwise]) if cond == *then => {
            let long = expand(then, dir, None)?;
            Some(if exists(&long) { long } else { expand(otherwise, dir, None)? })
        }
        _ => return None,
    };
    let mut line = last.strip_prefix('@').unwrap_or(last);
    // `get`: the line needn't be ASCII (a path with é, or U+FFFD from an OEM code page).
    if let Some(rest) = line.get(..NPM_PREFIX.len()).filter(|p| p.eq_ignore_ascii_case(NPM_PREFIX)).and(line.get(NPM_PREFIX.len()..)) {
        line = rest;
    }
    let words = line.trim_end().strip_suffix("%*")?;
    let argv = split(words)?.iter().map(|w| expand(w, dir, prog.as_deref())).collect::<Option<Vec<_>>>()?;
    (!argv.is_empty() && !argv[0].is_empty()).then_some(argv)
}

/// The words of a command line as cmd hands them on and the program's runtime splits them, or
/// None when anything in it means something to cmd (an unquoted `^ & | < > ( )`) or would split
/// differently in the program (a `\` before a `"`).
fn split(line: &str) -> Option<Vec<String>> {
    let (mut words, mut word, mut quoted, mut any) = (vec![], String::new(), false, false);
    let mut prev = ' ';
    for c in line.chars() {
        match c {
            '"' if prev == '\\' => return None,
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any {
                    words.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            '^' | '&' | '|' | '<' | '>' | '(' | ')' if !quoted => return None,
            c => {
                word.push(c);
                any = true;
            }
        }
        prev = c;
    }
    if quoted {
        return None;
    }
    if any {
        words.push(word);
    }
    Some(words)
}

/// `word` with the batch variables a shim uses replaced: `%~dp0` and npm's `%dp0%` (the shim's
/// folder, `dir`), npm's `%_prog%` and `%%`. None for any other `%`, whose value midna can't
/// know.
fn expand(word: &str, dir: &str, prog: Option<&str>) -> Option<String> {
    let mut out = String::new();
    let mut rest = word;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let lower = after.to_ascii_lowercase();
        let (value, len) = if lower.starts_with("%~dp0") || lower.starts_with("%dp0%") {
            (dir, 5)
        } else if lower.starts_with("%_prog%") {
            (prog?, 7)
        } else if after.starts_with("%%") {
            ("%", 2)
        } else {
            return None;
        };
        out.push_str(value);
        rest = &after[len..];
    }
    out.push_str(rest);
    // `%dp0%\node.exe` has two: keep a UNC path's leading pair, fold the rest.
    let lead = if out.starts_with("\\\\") { 2 } else { 0 };
    let mut folded = out[..lead].to_string();
    for c in out[lead..].chars() {
        if !(c == '\\' && folded.ends_with('\\') && folded.len() > lead) {
            folded.push(c);
        }
    }
    Some(folded)
}

/// The extensions [`which`] tries for a name without one (PATHEXT, else the usual four).
fn path_exts() -> Vec<String> {
    std::env::var("PATHEXT")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(|e| e.to_ascii_lowercase())
        .collect()
}

fn with_exts(p: &Path) -> Option<PathBuf> {
    if p.extension().is_some() && p.is_file() {
        return Some(p.to_path_buf());
    }
    path_exts().iter().map(|e| PathBuf::from(format!("{}{e}", p.display()))).find(|c| c.is_file())
}

/// Resolve a program against `path` (`;`-separated) and PATHEXT.
pub fn which(prog: &str, path: &str) -> Option<String> {
    let p = Path::new(prog);
    if prog.contains(['\\', '/', ':']) {
        return with_exts(p).map(|p| p.to_string_lossy().into_owned());
    }
    path.split(';').filter(|d| !d.is_empty()).find_map(|d| with_exts(&Path::new(d).join(prog))).map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = r"C:\Users\me\AppData\Roaming\npm\";

    /// What npm's cmd-shim (npm 10) writes for `claude`.
    const NPM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n";

    fn words(w: &[&str]) -> Option<Vec<String>> {
        Some(w.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn npm_shim_runs_node_on_the_packages_script() {
        let cli = r"C:\Users\me\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js";
        assert_eq!(parse(NPM, DIR, &|_| false), words(&["node", cli]));
        let beside = r"C:\Users\me\AppData\Roaming\npm\node.exe";
        assert_eq!(parse(NPM, DIR, &|p| p == beside), words(&[beside, cli]));
    }

    #[test]
    fn npm_shim_for_a_program_without_node() {
        let text = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\"%dp0%\\node_modules\\x\\bin\\x.exe\"   %*\r\n";
        assert_eq!(parse(text, DIR, &|_| false), words(&[r"C:\Users\me\AppData\Roaming\npm\node_modules\x\bin\x.exe"]));
    }

    #[test]
    fn one_line_shims() {
        assert_eq!(parse("@\"C:\\Git\\bin\\bash.exe\" \"C:\\t\\a b.sh\" %*\r\n", DIR, &|_| false), words(&[r"C:\Git\bin\bash.exe", r"C:\t\a b.sh"]));
        assert_eq!(parse("@echo off\npython \"%~dp0fake.py\" -u %*\n", r"C:\t\", &|_| false), words(&["python", r"C:\t\fake.py", "-u"]));
        assert_eq!(parse("@\"100%%.exe\" %*", DIR, &|_| false), words(&["100%.exe"]));
        // Not ASCII, with a character across the length of npm's prefix.
        let exe = format!(r"C:\{}.exe", "é".repeat(30));
        assert_eq!(parse(&format!("\"{exe}\" %*"), DIR, &|_| false), words(&[&exe]));
    }

    #[test]
    fn anything_else_stays_a_batch_file() {
        for text in [
            "",
            "@echo hi\r\n",
            "set FOO=1\r\nnode x.js %*\r\n",
            "node x.js %* --after\r\n",
            "node x.js %1 %2\r\n",
            "node %FOO%\\x.js %*\r\n",
            "a.exe | b.exe %*\r\n",
            "a.exe & b.exe %*\r\n",
            "a.exe ^& %*\r\n",
            "\"a.exe\" \"x\\\" %*\r\n",
            "\"a.exe %*\r\n",
            // corepack.cmd's older shape: two commands, cmd picks one.
            "@SETLOCAL\r\n@IF EXIST \"%~dp0\\node.exe\" (\r\n  \"%~dp0\\node.exe\"  \"%~dp0\\x.js\" %*\r\n) ELSE (\r\n  node  \"%~dp0\\x.js\" %*\r\n)\r\n",
            // npm's shim with variables from the script's shebang (`env -S`): not set here.
            &NPM.replace("CALL :find_dp0\r\n", "CALL :find_dp0\r\nSET \"NODE_OPTIONS=--max-old-space-size=4096\"\r\n"),
        ] {
            assert_eq!(parse(text, DIR, &|_| true), None, "{text:?}");
        }
    }

    #[test]
    fn resolve_follows_a_shim_to_its_program() {
        let dir = std::env::temp_dir().join(format!("midna-cmd-shim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = std::env::current_exe().unwrap();
        let shim = dir.join("agent.cmd");
        std::fs::write(&shim, format!("@\"{}\" --flag %*\r\n", exe.display())).unwrap();
        std::fs::write(dir.join("other.cmd"), "@echo %1\r\n").unwrap();
        let path = dir.to_string_lossy().into_owned();
        let argv = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let got = resolve(&argv(&["agent", "a\nb", "x|y"]), &path).unwrap();
        assert_eq!(got, argv(&[&exe.to_string_lossy(), "--flag", "a\nb", "x|y"]));
        let got = resolve(&argv(&["other", "1"]), &path).unwrap();
        assert!(got[0].to_ascii_lowercase().ends_with("other.cmd") && got[1] == "1", "{got:?}");
        assert!(resolve(&argv(&["surely-not-a-program-midna"]), &path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
