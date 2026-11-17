//! Where a program's bytes go, as a build analyzer shows it: an ELF file's **size** look
//! (readelf's sections and symbols) and a GNU ld map file's **usage** look (its memory
//! regions, sections, and what each object file and library puts in them). Each is a
//! read-only Markdown report (its parts in the Outline): bars for the regions' use (an
//! ELF borrows the regions of the map beside it), then the largest parts first.

use crate::panel::{App, Doc};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

/// Rows of a list (symbols, files) shown at most.
const TOP: usize = 200;
/// A bar's cells.
const BAR: usize = 24;

pub fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / f64::from(1 << 20)),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

fn bar(part: u64, whole: u64) -> String {
    let fill = if whole == 0 { 0 } else { ((part as f64 / whole as f64) * BAR as f64).round().min(BAR as f64) as usize };
    let fill = if part > 0 { fill.max(1) } else { 0 };
    format!("{}{}", "█".repeat(fill), "░".repeat(BAR - fill))
}

fn hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

/// A memory region of a linker script: its name, start and length.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub name: String,
    pub origin: u64,
    pub length: u64,
}

impl Region {
    fn holds(&self, addr: u64) -> bool {
        addr >= self.origin && addr - self.origin < self.length
    }
}

/// The regions' use: (region, bytes in it), as bars.
fn memory(lines: &mut Vec<String>, regions: &[Region], used: &HashMap<String, u64>) {
    let w = regions.iter().map(|r| r.name.len()).max().unwrap_or(4).max(4);
    for r in regions {
        let u = used.get(&r.name).copied().unwrap_or(0);
        let pct = if r.length == 0 { 0.0 } else { u as f64 * 100.0 / r.length as f64 };
        lines.push(format!("{:w$}  {}  {pct:5.1}%  {} of {} at 0x{:08x}", r.name, bar(u, r.length), human(u), human(r.length), r.origin));
    }
}

// ---------------------------------------------------------------- ELF

/// A section of an ELF file (readelf -S): name, address, size, flags, holds bytes.
#[derive(Clone, Debug, PartialEq)]
struct Section {
    name: String,
    addr: u64,
    size: u64,
    flags: String,
    bits: bool,
}

fn sections(text: &str) -> Vec<Section> {
    let mut out = Vec::new();
    let mut on = false;
    for l in text.lines() {
        if l.starts_with("Section Headers:") {
            on = true;
            continue;
        }
        if on && (l.starts_with("Key to Flags") || l.trim().is_empty() && !out.is_empty()) {
            break;
        }
        let Some((_, rest)) = l.split_once(']').filter(|_| on && l.trim_start().starts_with('[')) else { continue };
        let t: Vec<&str> = rest.split_whitespace().collect();
        // Name, type, address, offset, size, entry size, [flags,] link, info, align.
        if t.len() < 9 || t[0] == "Name" {
            continue;
        }
        let (Some(addr), Some(size)) = (hex(t[2]), hex(t[4])) else { continue };
        out.push(Section { name: t[0].to_string(), addr, size, flags: if t.len() >= 10 { t[6].to_string() } else { String::new() }, bits: t[1] != "NOBITS" });
    }
    out
}

/// A symbol with a size (readelf -s): its size, kind, name, section index.
fn symbols(text: &str) -> Vec<(u64, String, String, String)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // The full table when there is one, else the dynamic one.
    let table = if text.contains("Symbol table '.symtab'") { "Symbol table '.symtab'" } else { "Symbol table '.dynsym'" };
    let mut on = false;
    for l in text.lines() {
        if l.starts_with("Symbol table") {
            on = l.starts_with(table);
            continue;
        }
        let t: Vec<&str> = l.split_whitespace().collect();
        if !on || t.len() < 8 || !t[0].ends_with(':') {
            continue;
        }
        let size = t[2].strip_prefix("0x").map_or_else(|| t[2].parse().ok(), |h| u64::from_str_radix(h, 16).ok()).unwrap_or(0);
        if size == 0 || !matches!(t[3], "FUNC" | "OBJECT" | "TLS") {
            continue;
        }
        let name = t[7..].join(" ");
        if seen.insert((t[1].to_string(), name.clone())) {
            out.push((size, t[3].to_string(), name, t[6].to_string()));
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
    out
}

/// The machine readelf names, for whether Flash and RAM make sense.
fn machine(text: &str) -> String {
    text.lines().find_map(|l| l.trim().strip_prefix("Machine:")).map(|m| m.trim().to_string()).unwrap_or_default()
}

/// What readelf says about `path`: its header, sections, symbols (demangled).
fn readelf(path: &Path) -> Option<String> {
    let run = |demangle: bool| {
        let mut c = Command::new("readelf");
        c.args(["-W", "-h", "-S", "-s"]);
        if demangle {
            c.arg("-C");
        }
        c.arg("--").arg(path).stdin(Stdio::null()).stderr(Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    run(true).filter(|t| t.contains("Section Headers:")).or_else(|| run(false).filter(|t| t.contains("Section Headers:")))
}

/// The size report of ELF file `path` (named `name`); the regions of a map file beside
/// it (`<stem>.map`) make its bars.
pub fn elf_report(path: &Path, name: &str) -> Option<Vec<String>> {
    let text = readelf(path)?;
    let map = std::fs::read_to_string(path.with_extension("map")).ok().map(|m| regions(&m)).filter(|r| !r.is_empty());
    Some(elf_lines(name, &text, map.as_deref()))
}

fn elf_lines(name: &str, text: &str, regions: Option<&[Region]>) -> Vec<String> {
    let secs = sections(text);
    let alloc: Vec<&Section> = secs.iter().filter(|s| s.flags.contains('A') && s.size > 0).collect();
    let text_b: u64 = alloc.iter().filter(|s| s.bits && !s.flags.contains('W')).map(|s| s.size).sum();
    let data_b: u64 = alloc.iter().filter(|s| s.bits && s.flags.contains('W')).map(|s| s.size).sum();
    let bss_b: u64 = alloc.iter().filter(|s| !s.bits).map(|s| s.size).sum();
    let mach = machine(text);
    let mut lines = vec![format!("# {name}: size"), String::new()];
    lines.push(format!("{} · {}", if mach.is_empty() { "ELF" } else { &mach }, text.lines().find_map(|l| l.trim().strip_prefix("Type:")).map_or("", str::trim)));
    lines.push(String::new());
    lines.push("## Memory".into());
    lines.push(String::new());
    lines.push(format!("text {} · data {} · bss {}", human(text_b), human(data_b), human(bss_b)));
    let embedded = ["ARM", "RISC-V", "Xtensa", "Atmel AVR", "MSP430", "Tensilica"].iter().any(|m| mach.contains(m)) || regions.is_some();
    if embedded {
        lines.push(format!("Flash (text + data) {} · RAM (data + bss) {}", human(text_b + data_b), human(data_b + bss_b)));
    }
    if let Some(regions) = regions {
        // Each section in the region its address is in; data's bytes also in the first
        // region not holding it that it fits (where they load from).
        let mut used: HashMap<String, u64> = HashMap::new();
        for s in &alloc {
            if let Some(r) = regions.iter().find(|r| r.holds(s.addr)) {
                *used.entry(r.name.clone()).or_default() += s.size;
            }
        }
        if let Some(flash) = regions.iter().find(|r| r.holds(alloc.iter().filter(|s| s.flags.contains('X')).map(|s| s.addr).next().unwrap_or(u64::MAX))) {
            *used.entry(flash.name.clone()).or_default() += alloc.iter().filter(|s| s.bits && s.flags.contains('W') && !flash.holds(s.addr)).map(|s| s.size).sum::<u64>();
        }
        lines.push(String::new());
        memory(&mut lines, regions, &used);
        lines.push(String::new());
        lines.push("(regions from the map file beside it)".into());
    }
    lines.push(String::new());
    lines.push("## Sections".into());
    lines.push(String::new());
    let biggest = alloc.iter().map(|s| s.size).max().unwrap_or(1);
    let w = alloc.iter().map(|s| s.name.len()).max().unwrap_or(8).max(8);
    for s in &alloc {
        let kind = match (s.bits, s.flags.contains('W'), s.flags.contains('X')) {
            (false, ..) => "bss",
            (true, true, _) => "data",
            (true, false, true) => "code",
            _ => "read-only",
        };
        lines.push(format!("{:w$}  0x{:08x}  {:>9}  {}  {kind}", s.name, s.addr, human(s.size), bar(s.size, biggest)));
    }
    let syms = symbols(text);
    let index: HashMap<String, String> = {
        let mut m = HashMap::new();
        let mut on = false;
        for l in text.lines() {
            on |= l.starts_with("Section Headers:");
            if let (true, Some((n, rest))) = (on, l.trim_start().strip_prefix('[').and_then(|r| r.split_once(']'))) {
                if let Some(nm) = rest.split_whitespace().next() {
                    m.insert(n.trim().to_string(), nm.to_string());
                }
            }
        }
        m
    };
    lines.push(String::new());
    lines.push(format!("## Largest symbols ({} of {})", syms.len().min(TOP), syms.len()));
    lines.push(String::new());
    if syms.is_empty() {
        lines.push("No symbols with a size (stripped?).".into());
    }
    for (size, kind, name, ndx) in syms.iter().take(TOP) {
        let sec = index.get(ndx).map_or(ndx.as_str(), String::as_str);
        let kind = if kind == "FUNC" { "function" } else { "object" };
        lines.push(format!("{:>9}  {kind:<8}  {name}  ({sec})", human(*size)));
    }
    lines
}

// ---------------------------------------------------------------- map files

/// A map file's memory regions (its Memory Configuration).
pub fn regions(text: &str) -> Vec<Region> {
    let mut out = Vec::new();
    let mut lines = text.lines().skip_while(|l| !l.starts_with("Memory Configuration"));
    lines.next();
    for l in lines.by_ref() {
        if l.trim_start().starts_with("Name") {
            break;
        }
    }
    // The table, to the blank line after it (`*default*` alone: no regions).
    let mut rows = 0;
    for l in lines {
        let t: Vec<&str> = l.split_whitespace().collect();
        if t.is_empty() {
            if rows == 0 {
                continue;
            }
            break;
        }
        if l.starts_with("Linker script") {
            break;
        }
        rows += 1;
        if let (Some(name), Some(origin), Some(length)) = (t.first(), t.get(1).and_then(|o| hex(o)), t.get(2).and_then(|o| hex(o))) {
            if *name != "*default*" {
                out.push(Region { name: name.to_string(), origin, length });
            }
        }
    }
    out
}

/// A piece of the map: an input section put in an output section, its address, size
/// and file; or an output section (`file` empty) with its load address.
#[derive(Clone, Debug, PartialEq)]
struct Piece {
    out: String,
    name: String,
    addr: u64,
    size: u64,
    file: String,
    lma: Option<u64>,
}

/// Sections that never reach the target's memory.
fn kept_out(name: &str) -> bool {
    [".debug", ".comment", ".ARM.attributes", ".stab", ".gnu.attributes", ".riscv.attributes", ".xtensa.info", "/DISCARD/", ".note.GNU-stack"].iter().any(|p| name.starts_with(p))
}

/// The map's output sections and input pieces.
fn pieces(text: &str) -> (Vec<Piece>, Vec<Piece>) {
    let (mut outs, mut ins) = (Vec::new(), Vec::new());
    let Some(at) = text.find("Linker script and memory map") else { return (outs, ins) };
    let mut cur: Option<Piece> = None;
    // A name on a line of its own, its numbers on the next: (name, an output section).
    let mut pending: Option<(String, bool)> = None;
    for l in text[at..].lines().skip(1) {
        let t: Vec<&str> = l.split_whitespace().collect();
        if t.is_empty() {
            continue;
        }
        let top = !l.starts_with(char::is_whitespace);
        let input = l.starts_with(' ') && !l.starts_with("  ");
        let (name, nums): (String, &[&str]) = if top || input {
            (t[0].to_string(), &t[1..])
        } else if let Some((n, _)) = pending.as_ref() {
            (n.clone(), &t[..])
        } else {
            continue;
        };
        let is_out = if top || input { top } else { pending.as_ref().is_some_and(|p| p.1) };
        if (top || input) && nums.is_empty() {
            // Patterns (`*(.text*)`) and statements are not pieces.
            pending = (!name.starts_with('*') || name == "*fill*").then_some((name, top));
            continue;
        }
        pending = None;
        let (Some(addr), Some(size)) = (nums.first().and_then(|a| hex(a)), nums.get(1).and_then(|s| hex(s))) else { continue };
        if is_out {
            if ["LOAD", "OUTPUT(", "START", "END"].iter().any(|k| name.starts_with(k)) {
                continue;
            }
            let lma = l.split("load address").nth(1).and_then(|a| hex(a.trim()));
            let p = Piece { out: name.clone(), name, addr, size, file: String::new(), lma };
            outs.push(p.clone());
            cur = Some(p);
        } else if let Some(o) = cur.as_ref() {
            if size == 0 || name == "*fill*" || kept_out(&o.out) {
                continue;
            }
            let file = nums.get(2..).map(|f| f.join(" ")).unwrap_or_default();
            let lma = o.lma.map(|l| l + (addr - o.addr.min(addr)));
            ins.push(Piece { out: o.out.clone(), name, addr, size, file, lma });
        }
    }
    outs.retain(|o| !kept_out(&o.out) && o.size > 0);
    (outs, ins)
}

/// A piece's file as a library and its member (`libc.a(memcpy.o)`), or as itself.
fn library(file: &str) -> Option<&str> {
    let (lib, _) = file.split_once('(')?;
    Some(lib.rsplit('/').next().unwrap_or(lib))
}

/// An archive's member as `libc.a(memcpy.o)`, not the archive's folders.
fn short(file: &str) -> &str {
    match file.find('(') {
        Some(paren) => file[..paren].rfind('/').map_or(file, |s| &file[s + 1..]),
        None => file,
    }
}

/// The usage report of map file `name`.
pub fn map_lines(name: &str, text: &str) -> Vec<String> {
    let regions = regions(text);
    let (outs, ins) = pieces(text);
    let mut lines = vec![format!("# {name}: memory use"), String::new()];
    if outs.is_empty() {
        lines.push("Not a GNU ld map file (no \"Linker script and memory map\" in it).".into());
        return lines;
    }
    let region_of = |addr: u64| regions.iter().find(|r| r.holds(addr)).map(|r| r.name.clone());
    // Each piece in the region of its address, and of its load address when another.
    let mut used: HashMap<String, u64> = HashMap::new();
    for o in &outs {
        let (vma, lma) = (region_of(o.addr), o.lma.and_then(region_of));
        if let Some(r) = &vma {
            *used.entry(r.clone()).or_default() += o.size;
        }
        if let Some(r) = lma.filter(|l| Some(l) != vma.as_ref()) {
            *used.entry(r).or_default() += o.size;
        }
    }
    lines.push("## Memory".into());
    lines.push(String::new());
    if regions.is_empty() {
        lines.push("No memory regions in the map (no MEMORY in the linker script).".into());
    }
    memory(&mut lines, &regions, &used);
    lines.push(String::new());
    lines.push("## Sections".into());
    lines.push(String::new());
    let biggest = outs.iter().map(|o| o.size).max().unwrap_or(1);
    let w = outs.iter().map(|o| o.name.len()).max().unwrap_or(8).max(8);
    for o in &outs {
        let place = match (region_of(o.addr), o.lma.and_then(region_of)) {
            (Some(v), Some(l)) if v != l => format!("{v} (loaded from {l})"),
            (Some(v), _) => v,
            _ => String::new(),
        };
        lines.push(format!("{:w$}  0x{:08x}  {:>9}  {}  {place}", o.name, o.addr, human(o.size), bar(o.size, biggest)));
    }
    // By file: its bytes in each region.
    let names: Vec<String> = regions.iter().map(|r| r.name.clone()).collect();
    let mut by_file: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for p in &ins {
        let e = by_file.entry(if p.file.is_empty() { "(linker)".into() } else { p.file.clone() }).or_default();
        // Outside the regions (or with none): in the total only.
        *e.entry(region_of(p.addr).unwrap_or_default()).or_default() += p.size;
        if let Some(r) = p.lma.and_then(region_of).filter(|l| Some(l) != region_of(p.addr).as_ref()) {
            *e.entry(r).or_default() += p.size;
        }
    }
    let total = |m: &HashMap<String, u64>| m.values().sum::<u64>();
    // Each region's bytes, then the total.
    let columns = |m: &HashMap<String, u64>| names.iter().map(|n| format!("{:>9}", m.get(n).map_or("·".into(), |v| human(*v)))).chain([format!("{:>9}", human(total(m)))]).collect::<Vec<_>>().join("  ");
    let head = names.iter().chain([&"total".to_string()]).map(|n| format!("{n:>9}")).collect::<Vec<_>>().join("  ");
    let mut files: Vec<(&String, &HashMap<String, u64>)> = by_file.iter().collect();
    files.sort_by(|a, b| total(b.1).cmp(&total(a.1)).then(a.0.cmp(b.0)));
    lines.push(String::new());
    lines.push(format!("## By file ({} of {})", files.len().min(TOP), files.len()));
    lines.push(String::new());
    lines.push(format!("{head}  file"));
    for (f, m) in files.iter().take(TOP) {
        lines.push(format!("{}  {}", columns(m), short(f)));
    }
    // By library: its members together.
    let mut by_lib: HashMap<String, (HashMap<String, u64>, usize)> = HashMap::new();
    for (f, m) in &by_file {
        if let Some(lib) = library(f) {
            let e = by_lib.entry(lib.to_string()).or_default();
            for (r, v) in m.iter() {
                *e.0.entry(r.clone()).or_default() += v;
            }
            e.1 += 1;
        }
    }
    if !by_lib.is_empty() {
        let mut libs: Vec<_> = by_lib.iter().collect();
        libs.sort_by(|a, b| total(&b.1 .0).cmp(&total(&a.1 .0)).then(a.0.cmp(b.0)));
        lines.push(String::new());
        lines.push("## By library".into());
        lines.push(String::new());
        lines.push(format!("{head}  library"));
        for (lib, (m, n)) in libs {
            lines.push(format!("{}  {lib} ({n} file{})", columns(m), if *n == 1 { "" } else { "s" }));
        }
    }
    // The largest pieces: with -ffunction-sections, functions and objects by name.
    let mut big: Vec<&Piece> = ins.iter().collect();
    big.sort_by(|a, b| b.size.cmp(&a.size).then(a.name.cmp(&b.name)));
    lines.push(String::new());
    lines.push(format!("## Largest pieces ({} of {})", big.len().min(TOP), big.len()));
    lines.push(String::new());
    for p in big.iter().take(TOP) {
        let file = p.file.rsplit('/').next().unwrap_or(&p.file);
        lines.push(format!("{:>9}  {}  {file}", human(p.size), p.name));
    }
    lines
}

/// The usage look of map file `rel` (its open buffer's text, else the file's), made
/// again when that changes.
pub fn ensure(app: &mut App, key: &str, rel: &str) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match app.docs.get(&format!("file:{rel}")) {
        Some(Doc::Text(b)) => b.identity().hash(&mut h),
        _ => std::fs::metadata(app.root.join(rel)).ok().map(|m| (m.len(), m.modified().ok())).hash(&mut h),
    }
    let stamp = h.finish();
    if app.patch_stamps.get(key) == Some(&stamp) && app.docs.contains_key(key) {
        return;
    }
    let text = match app.docs.get(&format!("file:{rel}")) {
        Some(Doc::Text(b)) => b.lines.join("\n"),
        _ => std::fs::read_to_string(app.root.join(rel)).unwrap_or_default(),
    };
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let b = crate::editor::Buffer::from_lines("usage.md", map_lines(name, &text), "a map file's memory use, read-only");
    app.docs.insert(key.to_string(), Doc::Text(b));
    app.patch_stamps.insert(key.to_string(), stamp);
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = "Archive member included to satisfy reference by file (symbol)

Memory Configuration

Name             Origin             Length             Attributes
RAM              0x0000000020000000 0x0000000000005000 xrw
FLASH            0x0000000008000000 0x0000000000010000 xr
*default*        0x0000000000000000 0xffffffffffffffff

Linker script and memory map

LOAD Core/Src/main.o
                0x0000000020005000                _estack = (ORIGIN (RAM) + LENGTH (RAM))

.isr_vector     0x0000000008000000      0x10c
 *(.isr_vector)
 .isr_vector    0x0000000008000000      0x10c Core/Startup/startup.o

.text           0x0000000008000110     0x1000
 *(.text)
 .text          0x0000000008000110      0x200 /usr/lib/gcc/libc_nano.a(lib_a-memcpy.o)
 .text.HAL_RCC_OscConfig
                0x0000000008000310      0x600 Drivers/HAL/stm32f4xx_hal_rcc.o
                0x0000000008000310                HAL_RCC_OscConfig
 .text.main     0x0000000008000910      0x400 Core/Src/main.o
 *fill*         0x0000000008000d10      0x400

.data           0x0000000020000000       0x20 load address 0x0000000008001110
 .data.counter  0x0000000020000000       0x20 Core/Src/main.o

.bss            0x0000000020000020      0x100
 .bss           0x0000000020000020      0x100 /usr/lib/gcc/libc_nano.a(lib_a-impure.o)

.debug_info     0x0000000000000000     0x5000
 .debug_info    0x0000000000000000     0x5000 Core/Src/main.o
";

    #[test]
    fn a_map_file_reads_as_regions_sections_and_files() {
        assert_eq!(regions(MAP), [Region { name: "RAM".into(), origin: 0x2000_0000, length: 0x5000 }, Region { name: "FLASH".into(), origin: 0x0800_0000, length: 0x1_0000 }]);
        let (outs, ins) = pieces(MAP);
        assert_eq!(outs.iter().map(|o| (o.name.as_str(), o.size)).collect::<Vec<_>>(), [(".isr_vector", 0x10c), (".text", 0x1000), (".data", 0x20), (".bss", 0x100)], "debug sections are not memory");
        assert_eq!(outs[2].lma, Some(0x0800_1110));
        assert_eq!(ins.iter().map(|p| (p.name.as_str(), p.size)).collect::<Vec<_>>(), [(".isr_vector", 0x10c), (".text", 0x200), (".text.HAL_RCC_OscConfig", 0x600), (".text.main", 0x400), (".data.counter", 0x20), (".bss", 0x100)], "a name on its own line, its numbers on the next; no fill");
        let r = map_lines("fw.map", MAP);
        let at = |s: &str| r.iter().find(|l| l.starts_with(s)).cloned().unwrap_or_default();
        // FLASH: .isr_vector + .text + .data's load = 0x10c + 0x1000 + 0x20 = 4396 B of 64 KB.
        assert!(at("FLASH").contains("6.7%") && at("FLASH").contains("4.3 KB of 64.0 KB"), "{}", at("FLASH"));
        // RAM: .data + .bss = 0x120 of 20 KB.
        assert!(at("RAM ").contains("1.4%") && at("RAM ").contains("288 B"), "{}", at("RAM "));
        assert!(at(".data").ends_with("RAM (loaded from FLASH)"), "{}", at(".data"));
        assert!(r.iter().any(|l| l.contains("libc_nano.a (2 files)")), "{r:#?}");
        let big = r.iter().position(|l| l.starts_with("## Largest pieces")).unwrap();
        assert!(r[big + 2].contains(".text.HAL_RCC_OscConfig") && r[big + 2].contains("stm32f4xx_hal_rcc.o"), "{r:#?}");
        assert_eq!(map_lines("x.js.map", "{\"version\":3}")[2], "Not a GNU ld map file (no \"Linker script and memory map\" in it).");
        // A host program's map: no MEMORY, so no regions (not its sections taken for ones).
        let host = "Memory Configuration\n\nName             Origin             Length             Attributes\n*default*        0x0000000000000000 0xffffffffffffffff\n\nLinker script and memory map\n\n.interp         0x0000000000000374       0x1c\n .interp        0x0000000000000374       0x1c x.o\n";
        assert!(regions(host).is_empty());
        let r = map_lines("x.map", host);
        assert!(r.iter().any(|l| l.trim_start().starts_with("28 B  x.o")), "the total column\n{r:#?}");
    }

    #[test]
    fn an_elf_reads_as_sections_symbols_and_the_regions_of_its_map() {
        let text = "ELF Header:
  Type:                              EXEC (Executable file)
  Machine:                           ARM
Section Headers:
  [Nr] Name              Type            Addr     Off    Size   ES Flg Lk Inf Al
  [ 0]                   NULL            00000000 000000 000000 00      0   0  0
  [ 1] .isr_vector       PROGBITS        08000000 010000 00010c 00   A  0   0  1
  [ 2] .text             PROGBITS        08000110 010110 001000 00  AX  0   0 16
  [ 3] .data             PROGBITS        20000000 020000 000020 00  WA  0   0  4
  [ 4] .bss              NOBITS          20000020 020020 000100 00  WA  0   0  4
  [ 5] .comment          PROGBITS        00000000 020020 000043 01  MS  0   0  1
Key to Flags:

Symbol table '.symtab' contains 3 entries:
   Num:    Value  Size Type    Bind   Vis      Ndx Name
     1: 08000311  1536 FUNC    GLOBAL DEFAULT    2 HAL_RCC_OscConfig
     2: 20000000    32 OBJECT  LOCAL  DEFAULT    3 counter
     3: 08000911     0 FUNC    GLOBAL DEFAULT    2 empty
";
        let r = elf_lines("fw.elf", text, Some(&regions(MAP)));
        assert!(r.contains(&"text 4.3 KB · data 32 B · bss 256 B".to_string()), "{r:#?}");
        let flash = r.iter().find(|l| l.starts_with("FLASH")).unwrap();
        assert!(flash.contains("4.3 KB of 64.0 KB"), "data's load counted in Flash: {flash}");
        assert!(r.iter().any(|l| l.contains("1.5 KB  function  HAL_RCC_OscConfig  (.text)")), "{r:#?}");
        assert!(!r.iter().any(|l| l.contains("empty")), "no size, not listed");
        assert!(!r.iter().any(|l| l.starts_with(".comment")), "not in memory");
    }
}
