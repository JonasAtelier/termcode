//! ELF files (shared objects, executables, objects, kernel modules) open as what `readelf`
//! says about them: a summary first (its type and machine, SONAME, the libraries it needs,
//! the symbol versions it defines and needs, its build ID, the compiler), then readelf's
//! parts each under a heading, Markdown-style (the Outline and breadcrumbs list them). The
//! per-symbol version table (all numbers) is left out. Without readelf (binutils) the file
//! is a hexdump, as any binary.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long readelf may take before the file shows as a hexdump instead.
const READELF_WAIT: Duration = Duration::from_secs(5);

pub fn is_elf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
}

/// readelf's report on `path`; None without readelf, when it fails, or when it takes too
/// long (it is left to end on its own thread).
pub fn report(path: &Path) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let out = Command::new("readelf")
            .args(["-W", "-h", "-l", "-S", "-d", "-V", "-n", "--dyn-syms", "-p", ".comment", "--"])
            .arg(&path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(READELF_WAIT).ok()?.ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.contains("ELF Header:").then_some(text)
}

/// The text between the first `[` and `]` after `at` in `line`.
fn bracketed(line: &str, at: &str) -> Option<String> {
    let rest = &line[line.find(at)? + at.len()..];
    let a = rest.find('[')? + 1;
    let b = a + rest[a..].find(']')?;
    Some(rest[a..b].to_string())
}

/// The value after `key` on a `key: value` line of readelf's header.
fn field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim_start().strip_prefix(key).map(|v| v.trim().to_string()))
}

/// The lines shown for ELF file `name` from readelf's `text`.
pub fn view(name: &str, text: &str) -> Vec<String> {
    let class = field(text, "Class:").unwrap_or_default();
    let kind = field(text, "Type:").unwrap_or_default();
    let machine = field(text, "Machine:").unwrap_or_default();
    let all = |tag: &str| text.lines().filter(|l| l.contains(tag)).filter_map(|l| bracketed(l, tag)).collect::<Vec<_>>();
    let (soname, needed, runpath, rpath) = (all("(SONAME)"), all("(NEEDED)"), all("(RUNPATH)"), all("(RPATH)"));
    let interp = text.lines().find_map(|l| l.trim().strip_prefix("[Requesting program interpreter: ").and_then(|r| r.strip_suffix(']')).map(str::to_string));
    // The versions it defines (not its own name, the BASE one), and those it needs, by file.
    let (mut defines, mut needs): (Vec<String>, Vec<(String, Vec<String>)>) = (Vec::new(), Vec::new());
    let mut part = "";
    for l in text.lines() {
        if !l.starts_with(' ') && !l.is_empty() {
            part = if l.starts_with("Version definition section") {
                "def"
            } else if l.starts_with("Version needs section") {
                "need"
            } else {
                ""
            };
            continue;
        }
        let name_of = |l: &str| l.split("Name: ").nth(1).map(|n| n.split_whitespace().next().unwrap_or("").to_string());
        match part {
            "def" if l.contains("Rev:") && !l.contains("Flags: BASE") => defines.extend(name_of(l)),
            "need" if l.contains("File: ") => needs.push((l.split("File: ").nth(1).unwrap_or("").split_whitespace().next().unwrap_or("").to_string(), Vec::new())),
            "need" if l.contains("Name: ") => {
                if let (Some(n), Some(last)) = (name_of(l), needs.last_mut()) {
                    last.1.push(n);
                }
            }
            _ => {}
        }
    }
    let build_id = text.lines().find_map(|l| l.split("Build ID: ").nth(1).map(|b| b.trim().to_string()));
    let mut comment = Vec::new();
    let mut in_comment = false;
    for l in text.lines() {
        if !l.starts_with(' ') && !l.is_empty() {
            in_comment = l.starts_with("String dump of section '.comment'");
        } else if in_comment {
            if let Some((_, s)) = l.split_once(']') {
                let s = s.trim();
                if !s.is_empty() && !comment.contains(&s.to_string()) {
                    comment.push(s.to_string());
                }
            }
        }
    }
    let mut v = vec![format!("# {name}: {class} {kind}, {machine}"), String::new()];
    let mut item = |label: &str, value: String| {
        if !value.is_empty() {
            v.push(format!("- {label}: {value}"));
        }
    };
    item("SONAME", soname.join(", "));
    item("Needs", needed.join(", "));
    item("Runpath", runpath.join(", "));
    item("Rpath", rpath.join(", "));
    item("Interpreter", interp.unwrap_or_default());
    item("Versions it defines", defines.join(", "));
    for (file, names) in &needs {
        item(&format!("Versions it needs from {file}"), names.join(", "));
    }
    item("Build ID", build_id.unwrap_or_default());
    item("Built by", comment.join("; "));
    // Then readelf's parts, each a heading; the per-symbol version table is numbers only.
    let mut skip = false;
    for l in text.lines() {
        let top = !l.is_empty() && !l.starts_with(' ');
        if top {
            skip = l.starts_with("Version symbols section");
            if skip {
                continue;
            }
            if (l.ends_with(':') || l.starts_with("Displaying notes")) && !l.starts_with("Key to Flags") {
                v.push(String::new());
                v.push(format!("## {}", l.trim_end_matches(':')));
                continue;
            }
        }
        if !skip {
            v.push(l.to_string());
        }
    }
    while v.last().is_some_and(|l| l.trim().is_empty()) {
        v.pop();
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBZ: &str = "ELF Header:
  Class:                             ELF64
  Type:                              DYN (Shared object file)
  Machine:                           Advanced Micro Devices X86-64

Dynamic section at offset 0x1cdc0 contains 27 entries:
  Tag        Type                         Name/Value
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]
 0x000000000000000e (SONAME)             Library soname: [libz.so.1]
 0x000000000000001d (RUNPATH)            Library runpath: [$ORIGIN/../lib]

Version symbols section '.gnu.version' contains 124 entries:
 Addr: 0x0000000000001828  Offset: 0x00001828  Link: 3 (.dynsym)
  000:   0 (*local*)      10 (GLIBC_2.3.4)  11 (GLIBC_2.2.5)  11 (GLIBC_2.2.5)

Version definition section '.gnu.version_d' contains 3 entries:
 Addr: 0x0000000000001920  Offset: 0x00001920  Link: 4 (.dynstr)
  000000: Rev: 1  Flags: BASE  Index: 1  Cnt: 1  Name: libz.so.1
  0x001c: Rev: 1  Flags: none  Index: 2  Cnt: 1  Name: ZLIB_1.2.0
  0x0038: Rev: 1  Flags: none  Index: 3  Cnt: 2  Name: ZLIB_1.2.0.2
  0x0054: Parent 1: ZLIB_1.2.0

Version needs section '.gnu.version_r' contains 1 entry:
 Addr: 0x0000000000000bc8  Offset: 0x00000bc8  Link: 5 (.dynstr)
  000000: Version: 1  File: libc.so.6  Cnt: 2
  0x0010:   Name: GLIBC_2.14  Flags: none  Version: 9
  0x0020:   Name: GLIBC_2.2.5  Flags: none  Version: 2

Displaying notes found in: .note.gnu.build-id
  Owner                Data size \tDescription
  GNU                  0x00000014\tNT_GNU_BUILD_ID (unique build ID bitstring)\t    Build ID: a01740529db05d19952b4870bfbe06487a452ad6

String dump of section '.comment':
  [     0]  GCC: (Ubuntu 13.2.0-23ubuntu4) 13.2.0
";

    #[test]
    fn a_shared_librarys_summary_then_readelfs_parts_as_headings() {
        let v = view("libz.so.1.3", LIBZ);
        let has = |l: &str| v.iter().any(|x| x == l);
        assert_eq!(v[0], "# libz.so.1.3: ELF64 DYN (Shared object file), Advanced Micro Devices X86-64");
        for l in [
            "- SONAME: libz.so.1",
            "- Needs: libc.so.6",
            "- Runpath: $ORIGIN/../lib",
            "- Versions it defines: ZLIB_1.2.0, ZLIB_1.2.0.2",
            "- Versions it needs from libc.so.6: GLIBC_2.14, GLIBC_2.2.5",
            "- Build ID: a01740529db05d19952b4870bfbe06487a452ad6",
            "- Built by: GCC: (Ubuntu 13.2.0-23ubuntu4) 13.2.0",
            "## ELF Header",
            "## Dynamic section at offset 0x1cdc0 contains 27 entries",
            "## Displaying notes found in: .note.gnu.build-id",
        ] {
            assert!(has(l), "{l}: {v:#?}");
        }
        assert!(!v.iter().any(|l| l.contains("Version symbols section") || l.contains("(*local*)")), "the per-symbol table is left out");
    }

    #[test]
    fn readelf_reports_on_a_real_executable() {
        if Command::new("readelf").arg("--version").output().is_err() {
            return; // no binutils here
        }
        let exe = std::env::current_exe().unwrap();
        let text = report(&exe).expect("a report");
        let v = view("t", &text);
        assert!(v[0].starts_with("# t: ELF") && v.iter().any(|l| l == "## ELF Header"), "{:?}", &v[..3]);
    }
}
