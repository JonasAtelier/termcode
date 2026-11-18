//! STM32 Programmer: what STM32CubeProgrammer does, through its command line tool
//! (STM32_Programmer_CLI). The sidebar lists the features; the chosen one's page is the
//! `stm32:programmer` tab in the editor area (Memory a grid as CubeProgrammer's); the
//! tool's output and the running job's progress show in the panel under the editors,
//! beside the terminals, and so does the target's own output (its UART, through the
//! ST-LINK's virtual COM port: the Serial Console). Option bytes are read only. Without
//! the tool, the view offers to install it (ST's installer, run in the terminal).

use crate::gitcmds::{Input, Item, Pick};
use crate::panel::{App, Focus, PAct, Prompt};
use crate::theme::{icons, P};
use crate::ui::{button_style, fill, fit, fit_left, pill, row};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// The page's tab.
pub const KEY: &str = "programmer:page";

mod arduino;
mod idf;

/// The tool the Programmer runs (its switch at the top of the view): each takes the
/// targets it knows, given what it needs (a firmware file, a sketch, a project).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Family {
    Stm32,
    Arduino,
    Idf,
}
const FAMILIES: [(Family, &str); 3] = [(Family::Stm32, "STM32Cube"), (Family::Arduino, "Arduino"), (Family::Idf, "ESP-IDF")];

/// A job's command line: what runs, how it is shown, what the job is called.
pub(super) struct Cmdline {
    program: PathBuf,
    args: Vec<String>,
    what: String,
    tmp: Option<PathBuf>,
    shown: String,
}
const CLI: &str = "STM32_Programmer_CLI";
const DOWNLOAD: &str = "https://www.st.com/en/development-tools/stm32cubeprog.html";
/// The panel keeps this many lines of the tool's output.
const LOG_MAX: usize = 5000;
/// A job keeps this many of its lines to read when it ends.
const JOB_MAX: usize = 50_000;
/// Files the tool programs (its -d).
const FIRMWARE: [&str; 5] = ["bin", "hex", "elf", "srec", "s19"];
/// The most Memory reads at once (its hex dump is an editor's text).
const MAX_READ: u32 = 4 << 20;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Feature {
    Device,
    Memory,
    Program,
    Erase,
    OptionBytes,
    Serial,
}

/// The sidebar's rows: each feature, its name, what its page does.
pub const FEATURES: [(Feature, &str, &str); 6] = [
    (Feature::Device, "Device", "Connect to the target and read what it is."),
    (Feature::Memory, "Memory", "Read the target's memory: addresses, values, ASCII."),
    (Feature::Program, "Program", "Write a .bin, .hex, .elf, .srec or .s19 file to the flash."),
    (Feature::Erase, "Erase", "Erase the whole flash, or some of its sectors."),
    (Feature::OptionBytes, "Option Bytes", "The target's option bytes, read only."),
    (Feature::Serial, "Serial Console", "The target's UART output (its printf), through the ST-LINK's virtual COM port."),
];
/// Memory's data widths, in bytes.
const WIDTHS: [usize; 3] = [1, 2, 4];
const PORTS: [&str; 2] = ["SWD", "JTAG"];
/// Connection modes: shown, and the tool's name for each.
const MODES: [(&str, &str); 3] = [("Normal", "NORMAL"), ("Under reset", "UR"), ("Hot plug", "HOTPLUG")];
/// SWD/JTAG speeds: shown, and the tool's freq= (kHz; none: its default).
const FREQS: [(&str, Option<u32>); 5] = [("Auto", None), ("4 MHz", Some(4000)), ("1.8 MHz", Some(1800)), ("950 kHz", Some(950)), ("480 kHz", Some(480))];

/// What a run of the tool does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Op {
    Probes,
    Connect,
    Reset,
    ReadMem,
    Program,
    EraseChip,
    EraseSectors,
    ReadOb,
    /// Arduino: the boards arduino-cli sees; a sketch compiled, uploaded.
    Boards,
    Compile,
    Upload,
    /// ESP-IDF: idf.py build, flash, erase-flash, set-target.
    IdfBuild,
    IdfFlash,
    IdfErase,
    IdfTarget,
}

impl Op {
    fn family(self) -> Family {
        match self {
            Op::Boards | Op::Compile | Op::Upload => Family::Arduino,
            Op::IdfBuild | Op::IdfFlash | Op::IdfErase | Op::IdfTarget => Family::Idf,
            _ => Family::Stm32,
        }
    }

    /// It writes to the board through its serial port (the console lets go of it).
    fn uses_port(self) -> bool {
        matches!(self, Op::Upload | Op::IdfFlash | Op::IdfErase)
    }
}

/// A value typed in the input box.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Field {
    Addr,
    Size,
    File,
    BinAddr,
    Sectors,
    SaveAs,
    Baud,
    Send,
    /// Arduino: the board's FQBN.
    Fqbn,
}

#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Feature(usize),
    Install,
    Recheck,
    Port(usize),
    Mode(usize),
    ProbeRow(usize),
    Freq(usize),
    Run(Op),
    /// An erase: asked first.
    Confirm(Op),
    Ask(Field),
    Browse,
    Verify,
    FullErase,
    RunAfter,
    Disconnect,
    Width(usize),
    Tty,
    SerialToggle,
    SerialClear,
    PanelTerm,
    PanelLog,
    PanelSerial,
    Family(usize),
    ArdFeature(usize),
    Board(usize),
    ArdInstall,
    ArdRecheck,
    ArdVerify,
    BrowseSketch,
    IdfFeature(usize),
    IdfInstall,
    IdfRecheck,
    IdfTargetPick,
    BrowseProject,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub sn: String,
    pub fw: String,
    pub board: String,
}

/// An option bytes row: a group's title, or a name, its value and what it means.
#[derive(Clone, Debug, PartialEq)]
pub enum Ob {
    Group(String),
    Field(String, String, String),
}

struct Job {
    op: Op,
    what: String,
    since: Instant,
    pct: Option<u8>,
    out: Vec<String>,
    /// Memory's upload goes to this file first.
    tmp: Option<PathBuf>,
}

enum Msg {
    Line(String),
    Progress(u8),
    Done(bool),
    /// The serial port's, tagged with its connection.
    Serial(u64, crate::serial::Event),
}

pub struct Programmer {
    pub family: Family,
    pub ard: arduino::Arduino,
    pub idf: idf::Idf,
    /// The console let go of its port for an upload: it opens it again after.
    reopen: bool,
    /// What the file browser looks for.
    want: Want,
    pub cli: Option<PathBuf>,
    checked: bool,
    /// Install was pressed: the tool is looked for every few seconds.
    installing: Option<Instant>,
    pub feature: usize,
    pub port: usize,
    pub mode: usize,
    pub freq: usize,
    /// None until listed.
    pub probes: Option<Vec<Probe>>,
    pub probe: usize,
    /// Connect worked (the tool connects for each job anyway); Disconnect forgets the target.
    pub connected: bool,
    /// The target, as the last connection read it.
    pub info: Vec<(String, String)>,
    pub ob: Vec<Ob>,
    pub addr: u32,
    pub size: u32,
    /// The memory read: its address and bytes.
    pub mem: Option<(u32, Vec<u8>)>,
    /// Memory's data width, an index of WIDTHS.
    pub width: usize,
    pub file: Option<PathBuf>,
    /// The folder the file browser showed last.
    pub browsed: Option<PathBuf>,
    pub bin_addr: u32,
    pub verify: bool,
    pub full_erase: bool,
    pub run_after: bool,
    pub sectors: String,
    job: Option<Job>,
    /// A field edited on the page: which, and what is typed so far (it starts empty).
    pub editing: Option<(Field, String)>,
    /// Why the last value typed in a field was refused (red, under it).
    pub field_err: Option<(Field, String)>,
    /// The last job: whether it worked, and how it ended.
    pub last: Option<(bool, String)>,
    /// Its op (its page shows how it ended).
    last_op: Option<Op>,
    pub log: Vec<String>,
    /// The panel shows ours (the log, or with `serial_tab` the serial output), not a terminal.
    pub log_shown: bool,
    pub serial_tab: bool,
    /// The serial console: the port chosen, its rate, the open port, what came (the last
    /// line still coming).
    pub tty: Option<PathBuf>,
    pub baud: u32,
    pub serial: Option<crate::serial::Port>,
    serial_gen: u64,
    pub serial_out: Vec<String>,
    serial_off: usize,
    /// Lines the log is scrolled up from its end.
    log_off: usize,
    main_off: usize,
    main_area: Rect,
    runs: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    hits: Vec<(Rect, Hit)>,
    redrawn: Instant,
}

impl Default for Programmer {
    fn default() -> Self {
        let (tx, rx) = channel();
        Programmer {
            family: Family::Stm32,
            ard: Default::default(),
            idf: Default::default(),
            reopen: false,
            want: Want::Firmware,
            cli: None,
            checked: false,
            installing: None,
            feature: 0,
            port: 0,
            mode: 0,
            freq: 0,
            probes: None,
            probe: 0,
            connected: false,
            info: Vec::new(),
            ob: Vec::new(),
            addr: 0x0800_0000,
            size: 0x400,
            mem: None,
            width: 2,
            file: None,
            browsed: None,
            bin_addr: 0x0800_0000,
            verify: true,
            full_erase: false,
            run_after: true,
            sectors: String::new(),
            job: None,
            editing: None,
            field_err: None,
            last: None,
            last_op: None,
            log: Vec::new(),
            log_shown: false,
            serial_tab: false,
            tty: None,
            baud: 115200,
            serial: None,
            serial_gen: 0,
            serial_out: Vec::new(),
            serial_off: 0,
            log_off: 0,
            main_off: 0,
            main_area: Rect::default(),
            runs: 0,
            tx,
            rx,
            hits: Vec::new(),
            redrawn: Instant::now(),
        }
    }
}

impl Programmer {
    /// The panel has our tabs.
    pub fn has_log(&self) -> bool {
        !self.log.is_empty() || self.job.is_some() || self.has_serial()
    }

    /// Something comes in to show: a job's output, the console's, an install to notice.
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.serial.is_some() || self.installing.is_some() || self.ard.installing.is_some() || self.idf.installing.is_some()
    }

    pub fn busy_job(&self) -> bool {
        self.job.is_some()
    }

    fn has_serial(&self) -> bool {
        self.serial.is_some() || !self.serial_out.is_empty()
    }

    pub fn clear_hits(&mut self) {
        self.hits.clear();
    }
}

// ---------------------------------------------------------------- the tool

fn is_exe(p: &Path) -> bool {
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// STM32_Programmer_CLI: on PATH, where ST's installer puts it, or in STM32CubeIDE.
fn detect() -> Option<PathBuf> {
    if cfg!(test) {
        return None; // tests bring their own
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let ide = fs::read_dir("/opt/st").into_iter().flatten().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("stm32cubeide")).flat_map(|e| {
        fs::read_dir(e.path().join("plugins")).into_iter().flatten().flatten().filter(|p| p.file_name().to_string_lossy().contains("externaltools.cubeprogrammer")).map(|p| p.path().join("tools/bin"))
    });
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .chain([home.join("STMicroelectronics/STM32Cube/STM32CubeProgrammer/bin"), "/opt/st/stm32cubeprogrammer/bin".into(), "/usr/local/STMicroelectronics/STM32Cube/STM32CubeProgrammer/bin".into()])
        .chain(ide)
        .map(|d| d.join(CLI))
        .find(|p| is_exe(p))
}

fn ensure_checked(app: &mut App) {
    if !app.prog.checked {
        app.prog.checked = true;
        app.prog.cli = detect();
    }
}

/// A line of the tool's output as text: its colours dropped, no control characters.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => {
                if it.next() == Some('[') {
                    for c in it.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
            }
            '\t' => out.push(' '),
            c if !c.is_control() => out.push(c),
            _ => {}
        }
    }
    out.trim_end().to_string()
}

/// A progress bar's percentage: "[=====     ] 50%".
fn progress(s: &str) -> Option<u8> {
    let s = s.trim();
    let pct = |t: &str| t.trim().parse::<u8>().ok();
    // STM32_Programmer_CLI: "[=====     ] 50%".
    if let Some((bar, rest)) = s.rsplit_once(']').filter(|(b, _)| b.starts_with('[') && !b.contains('/')) {
        return pct(rest.trim().strip_suffix('%')?).filter(|_| !bar.is_empty());
    }
    // ninja (ESP-IDF, arduino-cli's ESP32 builds): "[123/1024] Building C object …".
    if let Some((n, _)) = s.strip_prefix('[').and_then(|r| r.split_once(']')) {
        let (a, b) = n.split_once('/')?;
        let (a, b): (u64, u64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        return (b > 0).then(|| (a * 100 / b).min(100) as u8);
    }
    // esptool: "Writing at 0x00010000... (45 %)".
    if s.starts_with("Writing at") {
        return pct(s.rsplit_once('(')?.1.strip_suffix("%)")?);
    }
    // avrdude: "Writing | ######## | 100% 0.29s".
    if s.starts_with("Writing |") || s.starts_with("Reading |") {
        return pct(s.rsplit_once('|')?.1.trim().split('%').next()?);
    }
    None
}

/// The first line saying what failed: a tool's "Error…", a compiler's ": error:".
fn first_error(out: &[String]) -> Option<String> {
    out.iter()
        .find(|l| {
            let t = l.trim_start().to_lowercase();
            t.starts_with("error") || t.contains(": error:") || t.starts_with("failed:") || t.starts_with("a fatal error occurred")
        })
        .cloned()
}

/// The tool with `args`, its output (stdout and stderr, as it prints them) sent line by
/// line, its progress bars as percentages; then whether it succeeded.
fn spawn(cli: PathBuf, args: Vec<String>, tx: Sender<Msg>) {
    std::thread::spawn(move || {
        let ok = run_cli(&cli, &args, &tx).unwrap_or_else(|e| {
            let _ = tx.send(Msg::Line(format!("Error: {}: {e}", cli.display())));
            false
        });
        let _ = tx.send(Msg::Done(ok));
    });
}

fn run_cli(cli: &Path, args: &[String], tx: &Sender<Msg>) -> std::io::Result<bool> {
    // "Text file busy": the tool (a script) still open for writing somewhere, e.g. in a
    // child another thread forked just then: a moment later it runs.
    let mut tries = 0;
    let (mut r, mut child) = loop {
        let (r, w) = std::io::pipe()?;
        let mut c = Command::new(cli);
        // Its own process group: Ctrl+C in a terminal never stops a flash half way.
        c.args(args).stdin(Stdio::null()).stdout(w.try_clone()?).stderr(w).process_group(0);
        match c.spawn() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            res => break (r, res?),
        }
    }; // the Command, and with it our ends of the pipe, gone: the read ends with the tool
    let emit = |seg: &[u8]| {
        let s = plain(&String::from_utf8_lossy(seg));
        if s.trim().is_empty() || s.trim().chars().all(|c| c == '-') {
            return;
        }
        let _ = tx.send(progress(&s).map_or(Msg::Line(s), Msg::Progress));
    };
    let mut read = || -> std::io::Result<()> {
        let (mut buf, mut part) = ([0u8; 4096], Vec::new());
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                emit(&part);
                return Ok(());
            }
            for &b in &buf[..n] {
                // A line ends at its newline, or at 64 KB (a tool printing binary).
                if b == b'\n' || b == b'\r' {
                    emit(&std::mem::take(&mut part));
                    continue;
                }
                if part.len() >= 64 << 10 {
                    emit(&std::mem::take(&mut part));
                }
                part.push(b);
            }
        }
    };
    let read = read();
    let ok = child.wait()?.success();
    read.map(|()| ok)
}

fn hex(n: u32) -> String {
    format!("0x{n:08X}")
}

fn size_text(n: usize) -> String {
    match n {
        n if n >= 1 << 20 && n % (1 << 20) == 0 => format!("{} MB", n >> 20),
        n if n >= 1 << 10 && n % (1 << 10) == 0 => format!("{} KB", n >> 10),
        n => format!("{n} bytes"),
    }
}

/// "0x08000000", "134217728", "16K", "2M".
fn num(s: &str) -> Option<u32> {
    let s = s.trim().replace('_', "");
    let (s, mul) = match s.chars().last() {
        Some('k' | 'K') => (&s[..s.len() - 1], 1 << 10),
        Some('m' | 'M') => (&s[..s.len() - 1], 1 << 20),
        _ => (&s[..], 1),
    };
    let n = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok()?,
        None => s.parse().ok()?,
    };
    n.checked_mul(mul)
}

/// Sectors as the tool's -e takes them: "0-3" a range, "0 1 2" (or commas) a list.
fn sector_args(s: &str) -> Option<Vec<String>> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once('-') {
        let (a, b): (u32, u32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        return (a <= b).then(|| vec![format!("[{a}"), format!("{b}]")]);
    }
    let v: Vec<String> = s.split([',', ' ']).filter(|x| !x.is_empty()).map(|x| x.parse::<u32>().ok().map(|n| n.to_string())).collect::<Option<_>>()?;
    (!v.is_empty()).then_some(v)
}

fn is_firmware(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| FIRMWARE.contains(&e.to_ascii_lowercase().as_str()))
}

/// A .bin has no address of its own: it is programmed at `bin_addr`.
fn is_bin(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("bin"))
}

fn conn_args(s: &Programmer) -> Vec<String> {
    let mut v = vec!["-c".to_string(), format!("port={}", PORTS[s.port]), format!("mode={}", MODES[s.mode].1)];
    if let Some(k) = FREQS[s.freq].1 {
        v.push(format!("freq={k}"));
    }
    if let Some(p) = s.probes.as_ref().filter(|p| p.len() > 1).and_then(|p| p.get(s.probe)) {
        v.push(format!("sn={}", p.sn));
    }
    v
}

/// The tool's arguments for `op`, what the job is called, and the file an upload goes to.
fn build(s: &Programmer, op: Op) -> Result<(Vec<String>, String, Option<PathBuf>), String> {
    let mut args = if matches!(op, Op::Probes | Op::Boards) { Vec::new() } else { conn_args(s) };
    let a = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut tmp = None;
    let what = match op {
        Op::Probes => {
            args.extend(a(&["-l", "stlink-only"]));
            "Looking for ST-LINK probes".to_string()
        }
        Op::Connect => "Connecting".into(),
        Op::Reset => {
            args.push("-rst".into());
            "Resetting the target".into()
        }
        Op::ReadMem => {
            let p = std::env::temp_dir().join(format!("termcode-stm32-{}-{}.bin", std::process::id(), s.runs + 1));
            args.extend(["-u".into(), hex(s.addr), hex(s.size), p.display().to_string()]);
            tmp = Some(p);
            format!("Reading {} at {}", size_text(s.size as usize), hex(s.addr))
        }
        Op::Program => {
            let f = s.file.as_ref().ok_or("Choose a file to program first")?;
            if s.full_erase {
                args.extend(a(&["-e", "all"]));
            }
            args.extend(["-d".into(), f.display().to_string()]);
            if is_bin(f) {
                args.push(hex(s.bin_addr));
            }
            if s.verify {
                args.push("-v".into());
            }
            if s.run_after {
                args.push("-rst".into());
            }
            format!("Programming {}", f.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()))
        }
        Op::EraseChip => {
            args.extend(a(&["-e", "all"]));
            "Erasing the full chip".into()
        }
        Op::EraseSectors => {
            let codes = sector_args(&s.sectors).ok_or("Type the sectors to erase first: 0-3, or 0 1 2")?;
            args.push("-e".into());
            args.extend(codes);
            format!("Erasing sectors {}", s.sectors.trim())
        }
        Op::ReadOb => {
            args.extend(a(&["-ob", "displ"]));
            "Reading the option bytes".into()
        }
        _ => return Err("not an STM32 job".into()), // the other tools build their own
    };
    Ok((args, what, tmp))
}

/// `args` as a command line to read (and paste).
fn shown_cmd(tool: &str, args: &[String]) -> String {
    let q = |a: &String| if a.contains(char::is_whitespace) { format!("'{a}'") } else { a.clone() };
    std::iter::once(tool.to_string()).chain(args.iter().map(q)).collect::<Vec<_>>().join(" ")
}

/// Run the tool for `op` (one job at a time); its output goes to the panel.
pub fn run(app: &mut App, op: Op) {
    if app.prog.job.is_some() {
        app.msg = "Programmer: a job is still running".into();
        return;
    }
    let s = &app.prog;
    let line = match op.family() {
        Family::Stm32 => match s.cli.clone() {
            None => Ok(None),
            Some(cli) => build(s, op).map(|(args, what, tmp)| Some(Cmdline { shown: shown_cmd(CLI, &args), program: cli, args, what, tmp })),
        },
        Family::Arduino => arduino::command(s, op),
        Family::Idf => idf::command(s, op),
    };
    let Cmdline { program, args, what, tmp, shown } = match line {
        Ok(Some(c)) => c,
        Ok(None) => return show(app),
        Err(e) => {
            app.msg = e;
            return;
        }
    };
    // An upload needs the port: the console lets go of it (and opens it again after).
    if op.uses_port() {
        let port = if op == Op::Upload { app.prog.ard.chosen().map(|b| PathBuf::from(&b.port)) } else { idf::flash_port(&app.prog) };
        let same = |a: &Path, b: &Path| fs::canonicalize(a).ok().zip(fs::canonicalize(b).ok()).is_some_and(|(x, y)| x == y);
        if let (Some(open), Some(port)) = (app.prog.serial.as_ref().map(|p| p.path.clone()), port) {
            if same(&open, &port) {
                serial_toggle(app);
                app.prog.reopen = true;
            }
        }
    }
    let s = &mut app.prog;
    s.runs += 1;
    s.log.push(format!("$ {shown}"));
    s.job = Some(Job { op, what, since: Instant::now(), pct: None, out: Vec::new(), tmp });
    (s.last, s.log_off, s.log_shown, s.serial_tab) = (None, 0, true, false);
    app.term_open = true;
    if app.focus == Focus::Term {
        app.focus = Focus::Editor;
    }
    spawn(program, args, app.prog.tx.clone());
}

/// A palette command: the view first when the tool is missing.
pub fn command(app: &mut App, op: Op) {
    if app.prog.family != Family::Stm32 {
        app.prog.family = Family::Stm32; // the palette's commands are STM32's
    }
    ensure_checked(app);
    if app.prog.cli.is_none() {
        return show(app);
    }
    run(app, op)
}

/// "Time elapsed during download operation: 00:00:01.250" as "1.3 s".
fn elapsed(out: &[String]) -> Option<String> {
    let t = out.iter().rev().find(|l| l.trim_start().starts_with("Time elapsed"))?.rsplit_once(": ")?.1.trim().to_string();
    let mut parts = t.split(':').rev();
    let secs: f64 = parts.next()?.parse().ok()?;
    let mins: u64 = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0) + 60 * parts.next().and_then(|h| h.parse::<u64>().ok()).unwrap_or(0);
    Some(if mins == 0 { format!("{secs:.1} s") } else { format!("{mins}:{:02.0}", secs) })
}

/// The job ended: what it read kept, how it went shown.
fn finish(app: &mut App, ok: bool) {
    let Some(job) = app.prog.job.take() else { return };
    let s = &mut app.prog;
    let err = first_error(&job.out);
    let ok = ok && err.is_none();
    let info = parse_info(&job.out);
    if !info.is_empty() {
        s.info = info;
    }
    // Any STM32 job but the probes' connects first.
    if job.op == Op::Connect {
        s.connected = ok;
    } else if ok && job.op.family() == Family::Stm32 && job.op != Op::Probes {
        s.connected = true;
    }
    match job.op {
        Op::Probes => {
            let p = parse_probes(&job.out);
            s.probe = s.probe.min(p.len().saturating_sub(1));
            s.probes = Some(p);
        }
        Op::ReadOb if ok => (s.ob, s.main_off) = (parse_ob(&job.out), 0),
        Op::ReadMem => {
            if let Some(p) = &job.tmp {
                if ok {
                    match fs::read(p) {
                        Ok(b) => s.mem = Some((s.addr, b)),
                        Err(e) => s.log.push(format!("Error: {}: {e}", p.display())),
                    }
                }
                let _ = fs::remove_file(p);
            }
        }
        _ => {}
    }
    let boards = (job.op == Op::Boards).then(|| arduino::boards_came(&mut s.ard, &job.out));
    let size = match job.op {
        Op::Compile | Op::Upload => arduino::summary(&job.out),
        Op::IdfBuild | Op::IdfFlash => idf::summary(&job.out),
        _ => None,
    };
    if job.op.family() == Family::Idf {
        idf::read_project(&mut s.idf);
    }
    let mut text = job.what.clone();
    if ok {
        text.push_str(": done");
        if let Some(t) = elapsed(&job.out) {
            text.push_str(&format!(" in {t}"));
        }
        if job.out.iter().any(|l| l.contains("verified successfully")) {
            text.push_str(", verified");
        }
        if let Some(b) = boards {
            text = b;
        }
        if let Some(x) = size {
            text.push_str(&format!(" · {x}"));
        }
        if job.op == Op::Probes {
            text = match s.probes.as_deref().unwrap_or_default().len() {
                0 => "No ST-LINK probe found".into(),
                1 => "Found an ST-LINK probe".into(),
                n => format!("Found {n} ST-LINK probes"),
            };
        }
    } else {
        let why = err.map_or("the tool failed".to_string(), |e| e.split_once(':').map_or(e.trim(), |x| x.1.trim()).to_string());
        text = format!("{} failed: {why}", job.what);
    }
    (s.last, s.last_op) = (Some((ok, text)), Some(job.op));
    if std::mem::take(&mut s.reopen) && s.serial.is_none() {
        serial_toggle(app);
    }
    let s = &mut app.prog;
    // The target started again: what it prints, if the console is open.
    if ok && s.serial.is_some() && (job.op == Op::Reset || (job.op == Op::Program && s.run_after)) {
        s.serial_tab = true;
    }
}

/// A line of the console's own ("── connected … ──").
fn serial_note(s: &mut Programmer, note: &str) {
    let rule = if crate::theme::ascii() { "--" } else { "──" };
    if s.serial_out.last().is_some_and(String::is_empty) {
        s.serial_out.pop();
    }
    s.serial_out.push(format!("{rule} {note} {rule}"));
    s.serial_out.push(String::new());
}

/// Text from the target: lines, the last one still coming.
fn serial_data(s: &mut Programmer, text: &str) {
    if s.serial_out.is_empty() {
        s.serial_out.push(String::new());
    }
    let before = s.serial_out.len();
    for c in text.chars() {
        match c {
            '\n' => s.serial_out.push(String::new()),
            '\r' => {}
            c => {
                // No newline coming (a wrong baud rate, binary data): lines of 4096 at most.
                if s.serial_out.last().is_some_and(|l| l.len() >= 4096) {
                    s.serial_out.push(String::new());
                }
                s.serial_out.last_mut().expect("a line").push(c);
            }
        }
    }
    if s.serial_off > 0 {
        s.serial_off += s.serial_out.len() - before; // a scrolled-up console stays put
    }
    if s.serial_out.len() > LOG_MAX {
        s.serial_out.drain(..s.serial_out.len() - LOG_MAX);
    }
}

/// The port the console opens unless one is chosen: the probe's own virtual COM port,
/// else any ST-LINK's, else the first.
fn default_tty(s: &Programmer) -> Option<PathBuf> {
    if s.family == Family::Arduino {
        if let Some(b) = s.ard.chosen() {
            return Some(PathBuf::from(&b.port));
        }
    }
    // ESP boards: their USB serial bridges (CP210x, CH34x) or their own USB, not an ST-LINK.
    if s.family == Family::Idf {
        let ports = crate::serial::ports();
        let esp = ["CP210", "CH34", "CH910", "Espressif", "JTAG", "1a86", "Silicon_Labs"];
        let pick = ports.iter().find(|(p, _)| esp.iter().any(|k| p.to_string_lossy().contains(k))).or_else(|| ports.iter().find(|(p, _)| !p.to_string_lossy().contains("STLink")));
        return pick.map(|(p, _)| p.clone());
    }
    let ports = crate::serial::ports();
    let sn = s.probes.as_ref().and_then(|p| p.get(s.probe)).map(|p| p.sn.clone()).filter(|sn| !sn.is_empty());
    let pick = |want: &dyn Fn(&str) -> bool| ports.iter().find(|(p, _)| want(&p.to_string_lossy())).map(|(p, _)| p.clone());
    sn.and_then(|sn| pick(&|p| p.contains(&sn))).or_else(|| pick(&|p| p.contains("STLink"))).or_else(|| ports.first().map(|(p, _)| p.clone()))
}

fn tty_name(p: &Path) -> String {
    let dev = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    dev.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())
}

/// Connect or Disconnect the console.
fn serial_toggle(app: &mut App) {
    let s = &mut app.prog;
    if let Some(port) = s.serial.take() {
        serial_note(s, &format!("disconnected from {}", tty_name(&port.path)));
        return;
    }
    let Some(path) = s.tty.clone().or_else(|| default_tty(s)) else {
        app.msg = "No serial port: plug the board in, or choose the port".into();
        return;
    };
    s.serial_gen += 1;
    let (tx, gen) = (s.tx.clone(), s.serial_gen);
    match crate::serial::Port::open(&path, s.baud, Box::new(move |e| drop(tx.send(Msg::Serial(gen, e))))) {
        Ok(port) => {
            serial_note(s, &format!("connected to {} at {} baud", tty_name(&path), s.baud));
            (s.tty, s.serial, s.serial_off) = (Some(path), Some(port), 0);
            (s.log_shown, s.serial_tab) = (true, true);
            app.term_open = true;
            if app.focus == Focus::Term {
                app.focus = Focus::Editor;
            }
        }
        Err(e) => app.msg = format!("{}: {e}", path.display()),
    }
}

fn pick_tty(app: &mut App) {
    let items: Vec<Item> = crate::serial::ports().into_iter().map(|(p, what)| Item { label: what, detail: p.display().to_string(), pick: Pick::Prog(Browse::Port(p)) }).collect();
    if items.is_empty() {
        app.msg = "No serial port: plug the board in".into();
        return;
    }
    crate::gitcmds::open_picker(app, "Serial port", items, None);
}

/// The tool's output, as the jobs run (tick).
pub fn tick(app: &mut App) -> bool {
    let mut changed = false;
    while let Ok(m) = app.prog.rx.try_recv() {
        changed = true;
        let s = &mut app.prog;
        match m {
            Msg::Line(l) => {
                if let Some(j) = s.job.as_mut() {
                    j.out.push(l.clone());
                    // Its start (what connected) and its end (how it went) are what count.
                    if j.out.len() > JOB_MAX {
                        j.out.drain(1000..j.out.len() - (JOB_MAX - 1000));
                    }
                }
                s.log.push(l);
                if s.log_off > 0 {
                    s.log_off += 1; // a scrolled-up log stays where it is
                }
            }
            Msg::Progress(p) => {
                if let Some(j) = s.job.as_mut() {
                    j.pct = Some(p);
                }
            }
            Msg::Done(ok) => finish(app, ok),
            Msg::Serial(gen, _) if gen != s.serial_gen => {}
            Msg::Serial(_, crate::serial::Event::Data(t)) => serial_data(s, &t),
            Msg::Serial(_, crate::serial::Event::End(why)) => {
                if let (Some(why), Some(port)) = (why, s.serial.take()) {
                    serial_note(s, &format!("{} closed: {why}", tty_name(&port.path)));
                }
            }
        }
    }
    let s = &mut app.prog;
    if s.log.len() > LOG_MAX {
        s.log.drain(..s.log.len() - LOG_MAX);
    }
    if s.installing.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) {
        s.installing = Some(Instant::now());
        if let Some(c) = detect() {
            (s.cli, s.installing) = (Some(c), None);
            app.msg = "STM32CubeProgrammer is installed".into();
            shown(app);
            return true;
        }
    }
    let a = &mut app.prog.ard;
    if a.installing.is_some_and(|t| t.elapsed() >= Duration::from_secs(2)) {
        a.installing = Some(Instant::now());
        if let Some(c) = arduino::detect() {
            (a.cli, a.installing) = (Some(c), None);
            app.msg = "arduino-cli is installed".into();
            arduino::shown(app);
            return true;
        }
    }
    let i = &mut app.prog.idf;
    if i.installing.is_some_and(|t| t.elapsed() >= Duration::from_secs(5)) {
        i.installing = Some(Instant::now());
        if let Some(r) = idf::detect() {
            (i.root, i.installing) = (Some(r), None);
            app.msg = "ESP-IDF is installed".into();
            idf::shown(app);
            return true;
        }
    }
    let s = &mut app.prog;
    // The running job's clock.
    if s.job.is_some() && s.redrawn.elapsed() >= Duration::from_millis(500) {
        s.redrawn = Instant::now();
        changed = true;
    }
    changed
}

// ---------------------------------------------------------------- reading the output

/// The target as a connection prints it ("Device name : STM32F76x/STM32F77x").
fn parse_info(out: &[String]) -> Vec<(String, String)> {
    const KEYS: [&str; 15] = ["Board", "ST-LINK FW", "Voltage", "SWD freq", "JTAG freq", "Connect mode", "Reset mode", "Device ID", "Revision ID", "Device name", "Flash size", "NVM size", "Device type", "Device CPU", "BL Version"];
    let mut v: Vec<(String, String)> = Vec::new();
    for l in out {
        if let Some((k, val)) = l.split_once(':') {
            let k = k.trim();
            if KEYS.contains(&k) && !v.iter().any(|(x, _)| x == k) {
                v.push((k.to_string(), val.trim().to_string()));
            }
        }
    }
    v
}

/// `-l stlink-only`: "ST-Link Probe 0 :", then its SN, FW, board.
fn parse_probes(out: &[String]) -> Vec<Probe> {
    let mut v: Vec<Probe> = Vec::new();
    for l in out {
        let l = l.trim();
        if l.starts_with("ST-Link Probe") {
            v.push(Probe::default());
            continue;
        }
        let (Some(p), Some((k, val))) = (v.last_mut(), l.split_once(':')) else { continue };
        let val = val.trim().to_string();
        match k.trim() {
            "ST-LINK SN" => p.sn = val,
            "ST-LINK FW" => p.fw = val,
            "Board Name" => p.board = val,
            _ => {}
        }
    }
    v
}

/// `-ob displ`: groups ("Read Out Protection:") and fields ("RDP : 0xAA (Level 0, …)").
fn parse_ob(out: &[String]) -> Vec<Ob> {
    let mut v = Vec::new();
    let mut started = false;
    for l in out {
        let t = l.trim();
        if let Some(bank) = t.strip_prefix("OPTION BYTES BANK:") {
            started = true;
            v.push(Ob::Group(format!("Bank {}", bank.trim())));
            continue;
        }
        if !started {
            continue;
        }
        if let Some(g) = t.strip_suffix(':') {
            v.push(Ob::Group(g.trim().to_string()));
        } else if let Some((k, val)) = t.split_once(" : ").or_else(|| t.split_once(':')) {
            let val = val.trim();
            let (value, meaning) = match val.split_once('(') {
                Some((a, b)) => (a.trim(), b.trim().strip_suffix(')').unwrap_or(b.trim())),
                None => (val, ""),
            };
            v.push(Ob::Field(k.trim().to_string(), value.to_string(), meaning.to_string()));
        }
    }
    v
}

// ---------------------------------------------------------------- the view and its page

/// The view was shown: the tool looked for; with it, the page and the probes.
pub fn shown(app: &mut App) {
    match app.prog.family {
        Family::Arduino => return arduino::shown(app),
        Family::Idf => return idf::shown(app),
        Family::Stm32 => {}
    }
    ensure_checked(app);
    if app.prog.cli.is_none() {
        return;
    }
    open_page(app);
    if app.prog.probes.is_none() && app.prog.job.is_none() {
        run(app, Op::Probes);
    }
}

/// The page's tab closed: back to the code, the Explorer beside it and the terminal under
/// it (a job still running goes on; its log stays in the panel).
pub fn closed(app: &mut App) {
    app.prog.editing = None;
    if app.side == crate::panel::Side::Programmer {
        app.side = crate::panel::Side::Explorer;
        if app.focus == Focus::Side {
            app.focus = Focus::Editor;
        }
        app.save_layout();
    }
    if app.prog.log_shown {
        app.prog.log_shown = false;
        if app.terms.is_empty() {
            app.hide_terminal();
        }
    }
}

fn show(app: &mut App) {
    app.show_side(crate::panel::Side::Programmer, false);
}

fn open_page(app: &mut App) {
    app.show_tab(app.ag, KEY.into(), false);
}

/// Is the active group's tab the page (the editors' Outline stays off).
pub fn active(app: &App) -> bool {
    app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).is_some_and(|t| t.key == KEY)
}

fn select(app: &mut App, i: usize) {
    app.prog.feature = i.min(FEATURES.len() - 1);
    app.prog.main_off = 0;
    // Program takes an open firmware file, unless one is chosen.
    if FEATURES[app.prog.feature].0 == Feature::Program && app.prog.file.is_none() {
        let open = app.groups.iter().flat_map(|g| g.tabs.iter()).filter_map(|t| t.key.strip_prefix("file:")).map(|rel| app.root.join(rel)).find(|p| is_firmware(p));
        app.prog.file = open;
    }
    open_page(app);
}

fn install(app: &mut App) {
    app.prog.log_shown = false;
    // ST's download needs a sign-in: its installer comes from ~/Downloads, or the page opens.
    let cmd = format!(
        "sh -c 'z=$(ls -t \"$HOME\"/Downloads/en.stm32cubeprg-lin*.zip 2>/dev/null | head -n 1); if [ -n \"$z\" ]; then d=$(mktemp -d) && unzip -q \"$z\" -d \"$d\" && \"$d\"/SetupSTM32CubeProgrammer*.linux; else echo \"Download STM32CubeProgrammer for Linux (ST asks you to sign in), then press Install again: {DOWNLOAD}\"; xdg-open {DOWNLOAD} >/dev/null 2>&1; fi'"
    );
    if app.run_in_new_terminal(&cmd) {
        app.prog.installing = Some(Instant::now());
    }
}

fn recheck(app: &mut App) {
    app.prog.cli = detect();
    app.prog.checked = true;
    if app.prog.cli.is_some() {
        app.prog.installing = None;
        shown(app);
    } else {
        app.msg = format!("{CLI} is still not found");
    }
}

/// The input box for `field`, empty (typing or pasting replaces the value); the value
/// now in its title.
fn ask(app: &mut App, field: Field) {
    let s = &app.prog;
    let title = match field {
        Field::Addr => format!("Address to read from, now {} (hex 0x…, or decimal)", hex(s.addr)),
        Field::Size => format!("Bytes to read, now {} (0x400, 1024, 16K, 1M)", hex(s.size)),
        Field::File => "File to program: a path (~/ and absolute too)".to_string(),
        Field::BinAddr => format!("Address a .bin file is programmed at, now {}", hex(s.bin_addr)),
        Field::Sectors => "Sectors to erase: a range (0-3) or a list (0 1 2)".to_string(),
        Field::Baud => format!("Baud rate, now {}", s.baud),
        Field::Send => "Send a line to the target (Enter sends it with a newline, Esc closes)".to_string(),
        Field::Fqbn => "The board's FQBN (e.g. arduino:avr:uno)".to_string(),
        Field::SaveAs => match &s.mem {
            Some((a, b)) => format!("Save the memory read as (in the folder unless absolute), e.g. memory-{}-{}.bin", hex(*a), b.len()),
            None => return,
        },
    };
    crate::gitcmds::open_picker(app, &title, Vec::new(), Some(Input::Prog(field)));
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `p` with the home folder as `~`.
fn tilde(p: &Path) -> String {
    match p.strip_prefix(home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// A typed or pasted path: quotes and file:// dropped, `~` the home folder, relative to
/// the workspace.
fn typed_path(root: &Path, t: &str) -> PathBuf {
    let t = t.trim().trim_matches(|c| c == '\'' || c == '"');
    let t = t.strip_prefix("file://").unwrap_or(t);
    match t.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ if Path::new(t).is_absolute() => PathBuf::from(t),
        _ => root.join(t),
    }
}

/// Enter in the input box: the value kept, or why not (the box stays).
pub fn entered(app: &mut App, field: Field, text: &str) -> Result<(), String> {
    let p = typed_path(&app.root, text);
    let s = &mut app.prog;
    match field {
        Field::Addr => s.addr = num(text).ok_or("Type an address: 0x08000000, or decimal")?,
        Field::BinAddr => s.bin_addr = num(text).ok_or("Type an address: 0x08000000, or decimal")?,
        Field::Size => s.size = num(text).filter(|n| (1..=MAX_READ).contains(n)).ok_or(format!("Type a size from 1 byte to {}", size_text(MAX_READ as usize)))?,
        Field::Sectors => {
            sector_args(text).ok_or("Type the sectors as a range (0-3) or a list (0 1 2)")?;
            s.sectors = text.trim().to_string();
        }
        Field::File => {
            if !p.is_file() {
                return Err(format!("{}: no such file", text.trim()));
            }
            if !is_firmware(&p) {
                return Err("The tool programs .bin, .hex, .elf, .srec and .s19 files".into());
            }
            s.file = Some(p);
        }
        Field::Baud => {
            let n = num(text).filter(|n| crate::serial::BAUDS.contains(n)).ok_or(format!("Type a rate: {}", crate::serial::BAUDS.map(|b| b.to_string()).join(", ")))?;
            s.baud = n;
            if s.serial.is_some() {
                serial_toggle(app);
                serial_toggle(app); // again at the new rate
            }
        }
        Field::Fqbn => {
            let t = text.trim();
            if t.split(':').filter(|p| !p.is_empty()).count() < 3 {
                return Err("Type an FQBN: vendor:architecture:board, e.g. arduino:avr:uno".into());
            }
            s.ard.fqbn = t.to_string();
        }
        Field::Send => {
            let port = s.serial.as_mut().ok_or("Connect the console first")?;
            port.send(&format!("{text}\n")).map_err(|e| e.to_string())?;
            ask(app, Field::Send); // the next line
        }
        Field::SaveAs => {
            let Some((_, bytes)) = &s.mem else { return Ok(()) };
            if text.trim().is_empty() || p.exists() {
                return Err(format!("{}: choose a name no file has", text.trim()));
            }
            fs::write(&p, bytes).map_err(|e| format!("{}: {e}", p.display()))?;
            app.msg = format!("saved {}", p.display());
        }
    }
    Ok(())
}

/// A row of the file browser.
#[derive(Clone, Debug)]
pub enum Browse {
    Type,
    Dir(PathBuf),
    File(PathBuf),
    /// A serial port for the console.
    Port(PathBuf),
    /// ESP-IDF: the chip set-target sets.
    Target(String),
}

/// What the file browser looks for: a firmware file, a sketch (its .ino), an ESP-IDF
/// project (its CMakeLists.txt); a file picked, the folder holding it for the last two.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Want {
    Firmware,
    Sketch,
    Project,
}

impl Want {
    fn takes(self, p: &Path) -> bool {
        match self {
            Want::Firmware => is_firmware(p),
            Want::Sketch => p.extension().is_some_and(|x| x == "ino"),
            Want::Project => p.file_name().is_some_and(|n| n == "CMakeLists.txt"),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Want::Firmware => "File to program",
            Want::Sketch => "Sketch (its .ino)",
            Want::Project => "ESP-IDF project (its CMakeLists.txt)",
        }
    }
}

/// The file browser: a folder's subfolders and firmware files, a click going into a
/// folder or choosing a file. It starts where it last was, else in the chosen file's
/// folder, else in the home folder.
fn browse(app: &mut App, dir: Option<PathBuf>) {
    let s = &app.prog;
    let want = s.want;
    let now = match want {
        Want::Firmware => s.file.clone(),
        Want::Sketch => s.ard.sketch.clone(),
        Want::Project => s.idf.project.clone(),
    };
    let dir = dir.or_else(|| s.browsed.clone()).or_else(|| now.as_ref().and_then(|f| f.parent().map(Path::to_path_buf))).unwrap_or_else(home);
    let entries = match fs::read_dir(&dir) {
        Ok(rd) => rd.flatten().collect::<Vec<_>>(),
        Err(e) => {
            app.msg = format!("{}: {e}", tilde(&dir));
            app.prog.browsed = None;
            return;
        }
    };
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            dirs.push(name);
        } else if want.takes(&path) {
            files.push((name, e.metadata().map_or(0, |m| m.len())));
        }
    }
    dirs.sort_by_key(|n| n.to_lowercase());
    files.sort_by_key(|(n, _)| n.to_lowercase());
    let item = |label: String, detail: String, b: Browse| Item { label, detail, pick: Pick::Prog(b) };
    let mut items = vec![item("Type a path…".into(), String::new(), Browse::Type)];
    if let Some(up) = dir.parent() {
        items.push(item(".. (up)".into(), tilde(up), Browse::Dir(up.to_path_buf())));
    }
    if dir != app.root {
        items.push(item("The workspace".into(), tilde(&app.root), Browse::Dir(app.root.clone())));
    }
    items.extend(dirs.into_iter().map(|n| item(format!("{n}/"), String::new(), Browse::Dir(dir.join(&n)))));
    let first_file = items.len();
    let chosen = if want == Want::Firmware { now } else { None };
    items.extend(files.into_iter().map(|(n, len)| item(n.clone(), size_text(len as usize), Browse::File(dir.join(&n)))));
    // The chosen file, else the first file, is selected.
    let sel = items.iter().position(|i| matches!(&i.pick, Pick::Prog(Browse::File(p)) if Some(p) == chosen.as_ref())).unwrap_or(if items.len() > first_file { first_file } else { 0 });
    app.prog.browsed = Some(dir.clone());
    crate::gitcmds::open_picker(app, &format!("{}: {}", want.title(), tilde(&dir)), items, None);
    if let Some(p) = app.gitui.cmds.picker.as_mut() {
        p.sel = sel;
    }
}

/// A row of the file browser picked.
pub fn picked(app: &mut App, b: Browse) {
    match b {
        Browse::Type => ask(app, Field::File),
        Browse::Dir(d) => browse(app, Some(d)),
        Browse::File(p) => match app.prog.want {
            Want::Firmware => app.prog.file = Some(p),
            Want::Sketch => app.prog.ard.sketch = p.parent().map(Path::to_path_buf),
            Want::Project => {
                app.prog.idf.project = p.parent().map(Path::to_path_buf);
                idf::read_project(&mut app.prog.idf);
            }
        },
        Browse::Target(t) => {
            app.prog.idf.want = t;
            confirm(app, Op::IdfTarget);
        }
        Browse::Port(p) => {
            let reopen = app.prog.serial.is_some();
            if reopen {
                serial_toggle(app);
            }
            app.prog.tty = Some(p);
            if reopen {
                serial_toggle(app);
            }
        }
    }
}

fn confirm(app: &mut App, op: Op) {
    let (text, button) = match op {
        Op::EraseChip => ("Erase the whole flash of the target?".to_string(), "Erase Chip"),
        Op::IdfErase => ("Erase the whole flash of the board (idf.py erase-flash)?".to_string(), "Erase Flash"),
        Op::IdfTarget => (format!("Set the target to {}? set-target deletes sdkconfig and the build folder.", app.prog.idf.want), "Set Target"),
        _ => (format!("Erase sectors {} of the target?", app.prog.sectors.trim()), "Erase Sectors"),
    };
    if op == Op::EraseSectors && sector_args(&app.prog.sectors).is_none() {
        return start_edit(app, Field::Sectors);
    }
    app.prompt = Some(Prompt { text: vec![text, "This can't be undone.".into()], buttons: vec![(button.into(), PAct::Prog(op)), ("Cancel".into(), PAct::Cancel)], sel: 1 });
}

// ---------------------------------------------------------------- input

/// A click on the view, the page or the panel's tabs. True when it was ours.
pub fn click(app: &mut App, p: Position) -> bool {
    let Some(h) = app.prog.hits.iter().rev().find(|(r, _)| r.contains(p)).map(|(_, h)| h.clone()) else { return false };
    // A click elsewhere takes the value being typed first; a bad one stays, with why.
    let same = matches!((&h, &app.prog.editing), (Hit::Ask(f), Some((g, _))) if f == g);
    if app.prog.editing.is_some() && !same && !commit(app, false) {
        return true;
    }
    let s = &mut app.prog;
    match h {
        Hit::Feature(i) => {
            select(app, i);
            app.focus = Focus::Side;
        }
        Hit::Install => install(app),
        Hit::Recheck => recheck(app),
        Hit::Port(i) => s.port = i,
        Hit::Mode(i) => s.mode = i,
        Hit::ProbeRow(i) => s.probe = i,
        Hit::Freq(i) => s.freq = i,
        Hit::Run(op) => run(app, op),
        Hit::Confirm(op) => confirm(app, op),
        Hit::Ask(f) if inline(f) => start_edit(app, f),
        Hit::Ask(f) => ask(app, f),
        Hit::Browse => {
            s.want = Want::Firmware;
            browse(app, None);
        }
        Hit::Verify => s.verify = !s.verify,
        Hit::FullErase => s.full_erase = !s.full_erase,
        Hit::RunAfter => s.run_after = !s.run_after,
        Hit::Disconnect => {
            (s.connected, s.last) = (false, Some((true, "Disconnected".into())));
            s.info.clear();
        }
        Hit::Width(i) => s.width = i,
        Hit::Tty => pick_tty(app),
        Hit::SerialToggle => serial_toggle(app),
        Hit::SerialClear => (s.serial_out, s.serial_off) = (Vec::new(), 0),
        Hit::PanelTerm => {
            s.log_shown = false;
            if app.terms.is_empty() {
                app.new_terminal();
            } else {
                app.focus = Focus::Term;
            }
        }
        Hit::Family(i) => {
            s.family = FAMILIES[i].0;
            s.editing = None;
            shown(app);
        }
        Hit::ArdFeature(i) => {
            s.ard.feature = i;
            if i == arduino::SKETCH {
                arduino::ensure_sketch(app);
            }
            open_page(app);
            app.focus = Focus::Side;
        }
        Hit::Board(i) => arduino::choose(&mut s.ard, i),
        Hit::ArdInstall => arduino::install(app),
        Hit::ArdRecheck => arduino::recheck(app),
        Hit::ArdVerify => s.ard.verify = !s.ard.verify,
        Hit::BrowseSketch => {
            s.want = Want::Sketch;
            browse(app, None);
        }
        Hit::IdfFeature(i) => {
            s.idf.feature = i;
            open_page(app);
            app.focus = Focus::Side;
        }
        Hit::IdfInstall => idf::install(app),
        Hit::IdfRecheck => idf::recheck(app),
        Hit::IdfTargetPick => idf::pick_target(app),
        Hit::BrowseProject => {
            s.want = Want::Project;
            browse(app, None);
        }
        Hit::PanelLog | Hit::PanelSerial => {
            (s.log_shown, s.serial_tab) = (true, h == Hit::PanelSerial);
            if app.focus == Focus::Term {
                app.focus = Focus::Editor;
            }
        }
    }
    true
}

/// The wheel over the log or the page.
pub fn wheel(app: &mut App, p: Position, d: isize) -> bool {
    let s = &mut app.prog;
    if s.log_shown && app.term_open && app.ui.term.contains(p) {
        let (off, len) = if s.serial_tab { (&mut s.serial_off, s.serial_out.len()) } else { (&mut s.log_off, s.log.len()) };
        *off = off.saturating_add_signed(-d).min(len.saturating_sub(1));
        return true;
    }
    if active(app) && app.prog.main_area.contains(p) {
        app.prog.main_off = app.prog.main_off.saturating_add_signed(d); // the page clamps it
        return true;
    }
    false
}

/// Fields typed in on the page itself (the others ask in a box).
fn inline(f: Field) -> bool {
    matches!(f, Field::Addr | Field::Size | Field::BinAddr | Field::Sectors | Field::Baud | Field::Fqbn)
}

/// Typing in field `f`, from empty (Esc keeps the value it had).
fn start_edit(app: &mut App, f: Field) {
    app.prog.editing = Some((f, String::new()));
    app.prog.field_err = None;
    app.focus = Focus::Editor;
}

/// The value typed taken (true), or refused with why under the field (false). Enter in
/// Memory's Address or Size reads the memory there (`enter`).
fn commit(app: &mut App, enter: bool) -> bool {
    let Some((f, text)) = app.prog.editing.take() else { return true };
    match entered(app, f, &text) {
        Ok(()) => {
            app.prog.field_err = None;
            if enter && matches!(f, Field::Addr | Field::Size) && app.prog.job.is_none() {
                run(app, Op::ReadMem);
            }
            true
        }
        Err(e) => {
            app.prog.editing = Some((f, text));
            app.prog.field_err = Some((f, e));
            false
        }
    }
}

/// A paste into the field being typed in (its first line). False when none is.
pub fn paste(app: &mut App, text: &str) -> bool {
    if !active(app) {
        return false;
    }
    let Some((_, t)) = app.prog.editing.as_mut() else { return false };
    t.push_str(text.lines().next().unwrap_or("").trim());
    true
}

/// Keys on the page: a field being typed in takes them; else they scroll Memory's grid
/// and the option bytes.
pub fn page_key(app: &mut App, k: KeyEvent) -> bool {
    if !active(app) {
        return false;
    }
    if let Some((_, t)) = app.prog.editing.as_mut() {
        let ctrl = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match k.code {
            KeyCode::Char(c) if !ctrl => t.push(c),
            KeyCode::Backspace => {
                t.pop();
            }
            KeyCode::Enter => {
                commit(app, true);
            }
            KeyCode::Esc => (app.prog.editing, app.prog.field_err) = (None, None),
            _ => {}
        }
        return true;
    }
    let s = &mut app.prog;
    let page = (s.main_area.height as usize).saturating_sub(1).max(1);
    s.main_off = match k.code {
        KeyCode::Up => s.main_off.saturating_sub(1),
        KeyCode::Down => s.main_off + 1,
        KeyCode::PageUp => s.main_off.saturating_sub(page),
        KeyCode::PageDown => s.main_off + page,
        KeyCode::Home => 0,
        KeyCode::End => usize::MAX / 2, // the page clamps it
        KeyCode::F(5) => {
            primary(app);
            return true;
        }
        _ => return false,
    };
    true
}

/// F5: the page's main action (an erase is clicked, and asked, only).
fn primary(app: &mut App) {
    match app.prog.family {
        Family::Arduino => return arduino::primary(app),
        Family::Idf => return idf::primary(app),
        Family::Stm32 => {}
    }
    match FEATURES[app.prog.feature].0 {
        Feature::Device => run(app, Op::Connect),
        Feature::Memory => run(app, Op::ReadMem),
        Feature::Program => run(app, Op::Program),
        Feature::OptionBytes => run(app, Op::ReadOb),
        Feature::Serial => serial_toggle(app),
        Feature::Erase => {}
    }
}

/// Keys in the view: ↑↓ choose a feature, Enter goes to its page.
pub fn key(app: &mut App, k: KeyEvent) -> bool {
    match app.prog.family {
        Family::Arduino => return arduino::key(app, k),
        Family::Idf => return idf::key(app, k),
        Family::Stm32 => {}
    }
    if app.prog.cli.is_none() {
        if k.code == KeyCode::Enter {
            install(app);
            return true;
        }
        return false;
    }
    let i = app.prog.feature;
    match k.code {
        KeyCode::Up => select(app, i.saturating_sub(1)),
        KeyCode::Down => select(app, i + 1),
        KeyCode::Enter | KeyCode::Char(' ') => {
            select(app, i);
            app.focus = Focus::Editor;
        }
        KeyCode::F(5) => primary(app),
        _ => return false,
    }
    true
}

// ---------------------------------------------------------------- drawing

/// The page's glyphs, as `icons().prog` holds them.
const I_CONNECT: usize = 0;
const I_DISCONNECT: usize = 1;
const I_PROGRAM: usize = 2;
const I_REFRESH: usize = 3;
const I_ERASE: usize = 4;
const I_SAVE: usize = 5;
const I_READ: usize = 6;
const I_FILE: usize = 7;
const I_RESET: usize = 8;
const I_SHIELD: usize = 9;
const I_TERMINAL: usize = 10;
const I_MEMORY: usize = 11;
const I_FOLDER: usize = 12;
const I_SEND: usize = 13;

fn glyph(i: usize) -> &'static str {
    icons().prog[i]
}

fn feature_icon(f: Feature) -> &'static str {
    match f {
        Feature::Device => icons().chip,
        Feature::Memory => glyph(I_MEMORY),
        Feature::Program => glyph(I_PROGRAM),
        Feature::Erase => glyph(I_ERASE),
        Feature::OptionBytes => glyph(I_SHIELD),
        Feature::Serial => glyph(I_TERMINAL),
    }
}

/// The feature a job belongs to (its row shows it running).
fn feature_of(op: Op) -> Feature {
    match op {
        Op::Probes | Op::Connect | Op::Reset => Feature::Device,
        Op::Boards | Op::Compile | Op::Upload | Op::IdfBuild | Op::IdfFlash | Op::IdfErase | Op::IdfTarget => Feature::Device, // not STM32's
        Op::ReadMem => Feature::Memory,
        Op::Program => Feature::Program,
        Op::EraseChip | Op::EraseSectors => Feature::Erase,
        Op::ReadOb => Feature::OptionBytes,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Primary,
    Secondary,
    Danger,
}

/// A pill button (its icon, its label) on `under`; not `on`: greyed, no click. Drawn whole
/// or not at all. Returns the x after it.
#[allow(clippy::too_many_arguments)]
fn button(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), icon: &str, label: &str, look: Look, on: bool, hit: Hit, under: ratatui::style::Color) -> u16 {
    let text = if icon.is_empty() { label.to_string() } else { format!("{icon} {label}") };
    let w = text.width() as u16 + 2;
    let r = Rect::new(x, y, w, 1);
    if r.right() > b.right() || y < b.y || y >= b.bottom() {
        return x + w + 1;
    }
    let hot = on && app.hover.is_some_and(|p| r.contains(p));
    let st = match (on, look) {
        (false, _) => Style::new().fg(P.dim).bg(P.button2_bg),
        (true, Look::Primary) => button_style(true, hot),
        (true, Look::Secondary) => button_style(false, hot),
        (true, Look::Danger) => Style::new().fg(P.button_fg).bg(P.error_fg).add_modifier(if hot { Modifier::BOLD } else { Modifier::empty() }),
    };
    f.render_widget(Line::from(pill(text, st, under)), r);
    if on {
        app.prog.hits.push((r, hit));
    }
    x + w + 1
}

/// A key's name, dim, after a button.
fn key_hint(f: &mut Frame, b: Rect, (x, y): (u16, u16), key: &str) {
    let r = Rect::new(x, y, key.width() as u16, 1);
    if r.right() <= b.right() && y < b.bottom() {
        f.render_widget(Span::styled(key.to_string(), Style::new().fg(P.dim)), r);
    }
}

/// A segmented control: the choices side by side in one pill, the chosen one lit.
#[allow(clippy::too_many_arguments)]
fn segmented(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), opts: &[String], sel: usize, hit: fn(usize) -> Hit, under: ratatui::style::Color) -> u16 {
    if y < b.y || y >= b.bottom() {
        return x;
    }
    let nerd = crate::theme::nerd();
    let mut cx = x + u16::from(nerd);
    let bgs: Vec<_> = opts
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let r = Rect::new(cx, y, o.width() as u16 + 2, 1);
            cx += r.width;
            let hot = app.hover.is_some_and(|p| r.contains(p));
            let st = if i == sel {
                Style::new().fg(P.button_fg).bg(P.button_bg).bold()
            } else if hot {
                Style::new().fg(P.editor_fg).bg(P.list_hover_bg)
            } else {
                Style::new().fg(P.input_fg).bg(P.input_bg)
            };
            (r, st, format!(" {o} "))
        })
        .collect();
    if cx > b.right() {
        return cx + 2;
    }
    for (i, (r, st, t)) in bgs.iter().enumerate() {
        f.render_widget(Span::styled(t.clone(), *st), *r);
        app.prog.hits.push((*r, hit(i)));
    }
    if nerd {
        let cap = |c: &'static str, st: Style| Span::styled(c, Style::new().fg(st.bg.unwrap_or(P.input_bg)).bg(under));
        f.render_widget(cap("\u{e0b6}", bgs[0].1), Rect::new(x, y, 1, 1));
        f.render_widget(cap("\u{e0b4}", bgs[bgs.len() - 1].1), Rect::new(cx, y, 1, 1));
    }
    cx + 2
}

#[allow(clippy::too_many_arguments)]
fn field(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), w: u16, text: &str, placeholder: &str, hit: Hit) -> u16 {
    let r = Rect::new(x, y, w, 1).intersection(b);
    if !r.is_empty() {
        let hot = app.hover.is_some_and(|p| r.contains(p));
        let room = r.width.saturating_sub(2) as usize;
        let typing = match (&hit, &app.prog.editing) {
            (Hit::Ask(f), Some((g, t))) if f == g => Some(t.clone()),
            _ => None,
        };
        if let Some(t) = &typing {
            let shown = fit_left(t, room);
            row(f, r, P.input_bg, vec![Span::styled(format!(" {shown}"), Style::new().fg(P.input_fg))]);
            // Typing shows: the field's edge in the focus colour, the caret after the text.
            f.render_widget(Span::styled("▏", Style::new().fg(P.focus_border).bg(P.input_bg)), Rect::new(r.x, r.y, 1, 1));
            if app.focus == Focus::Editor {
                f.set_cursor_position((r.x + 1 + (shown.width() as u16).min(r.width.saturating_sub(2)), r.y));
            }
        } else {
            let (t, fg) = if text.is_empty() { (fit(placeholder, room), P.placeholder) } else { (fit_left(text, room), P.input_fg) };
            row(f, r, if hot { P.list_hover_bg } else { P.input_bg }, vec![Span::styled(format!(" {t}"), Style::new().fg(fg))]);
        }
        let err = match (&hit, &app.prog.field_err) {
            (Hit::Ask(f), Some((g, e))) if f == g => Some(e.clone()),
            _ => None,
        };
        if let Some(e) = err.filter(|_| y + 1 < b.bottom()) {
            let er = Rect::new(r.x, y + 1, b.right().saturating_sub(r.x), 1);
            f.render_widget(Span::styled(fit(&e, er.width as usize), Style::new().fg(P.error_fg)), er);
        }
        app.prog.hits.push((r, hit));
    }
    x + w + 1
}

fn check(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), on: bool, label: &str, hit: Hit) -> u16 {
    let mark = if crate::theme::ascii() {
        Span::styled(if on { "[x]" } else { "[ ]" }, Style::new().fg(P.editor_fg))
    } else if on {
        Span::styled(format!(" {} ", icons().check), Style::new().fg(P.button_fg).bg(P.button_bg))
    } else {
        Span::styled("   ", Style::new().bg(P.input_bg))
    };
    let w = 4 + label.width() as u16;
    let r = Rect::new(x, y, w, 1).intersection(b);
    if !r.is_empty() {
        let hot = app.hover.is_some_and(|p| r.contains(p));
        let st = Style::new().fg(P.editor_fg).add_modifier(if hot { Modifier::UNDERLINED } else { Modifier::empty() });
        f.render_widget(Line::from(vec![mark, Span::raw(" "), Span::styled(label.to_string(), st)]), r);
        app.prog.hits.push((r, hit));
    }
    x + w + 2
}

/// A dim label at `x`, `w` wide.
fn label(f: &mut Frame, b: Rect, (x, y): (u16, u16), text: &str) {
    let r = Rect::new(x, y, (text.width() as u16).min(b.right().saturating_sub(x)), 1);
    if y < b.bottom() {
        f.render_widget(Span::styled(text.to_string(), Style::new().fg(P.dim)), r);
    }
}

/// Text at (x, y), cut to `b`.
fn text_at(f: &mut Frame, b: Rect, (x, y): (u16, u16), spans: Vec<Span<'static>>) {
    if y >= b.y && y < b.bottom() && x < b.right() {
        f.render_widget(Line::from(spans), Rect::new(x, y, b.right() - x, 1));
    }
}

/// A titled card with rounded corners; returns inside it (a cell of padding each side).
fn card(f: &mut Frame, r: Rect, title: &str, border: ratatui::style::Color) -> Rect {
    if r.height < 3 || r.width < 8 {
        return Rect::default();
    }
    let blk = crate::ui::popup_block(P.editor_bg, P.editor_fg, (1, 0))
        .border_style(Style::new().fg(border).bg(P.editor_bg))
        .title(Line::from(Span::styled(format!(" {title} "), Style::new().fg(P.editor_fg).bold())));
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    inner
}

/// A scrollbar on `track` for `total` rows, `view` shown from `off`.
fn scrollbar(f: &mut Frame, track: Rect, total: usize, view: usize, off: usize) {
    if total <= view || track.height == 0 {
        return;
    }
    let h = track.height as usize;
    let len = (h * view / total).clamp(1, h);
    let top = (h - len) * off / (total - view).max(1);
    for i in 0..h {
        let on = (top..top + len).contains(&i);
        let g = if crate::theme::ascii() { if on { "#" } else { "|" } } else if on { "┃" } else { "│" };
        f.render_widget(Span::styled(g, Style::new().fg(if on { P.dim } else { P.widget_border })), Rect::new(track.x, track.y + i as u16, 1, 1));
    }
}

/// "3 min ago" for a file's change.
fn ago(t: std::time::SystemTime) -> String {
    let s = t.elapsed().map_or(0, |d| d.as_secs());
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

fn probe_text(s: &Programmer) -> String {
    match s.probes.as_deref() {
        None if s.job.as_ref().is_some_and(|j| j.op == Op::Probes) => "Looking for ST-LINK probes…".into(),
        None => "Not looked for yet".into(),
        Some([]) => "No ST-LINK probe found".into(),
        Some(ps) => probe_line(&ps[s.probe.min(ps.len() - 1)]),
    }
}

fn probe_line(p: &Probe) -> String {
    let board = if p.board.is_empty() || p.board == "--" { String::new() } else { format!("{} · ", p.board) };
    format!("{board}ST-LINK {} · SN {}", p.fw, p.sn)
}

fn info<'a>(s: &'a Programmer, k: &str) -> Option<&'a str> {
    s.info.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str())
}

/// The view: the target, the features, the probe and the tool; or what to do without it.
pub fn draw_side(app: &mut App, f: &mut Frame, r: Rect) {
    ensure_checked(app);
    let focused = app.focus == crate::panel::Focus::Side && !app.activity_lit;
    let card_r = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused);
    let pad = (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1);
    let area = crate::ui::inset(card_r, pad);
    app.ui.side_list = Rect::default();
    if area.width < 8 || area.height < 4 {
        return;
    }
    // The switch: what the view programs.
    let fams: Vec<String> = FAMILIES.iter().map(|(_, n)| n.to_string()).collect();
    let sel = FAMILIES.iter().position(|(f, _)| *f == app.prog.family).unwrap_or(0);
    segmented(app, f, area, (area.x, area.y), &fams, sel, Hit::Family, P.sidebar_bg);
    let area = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
    match app.prog.family {
        Family::Arduino => return arduino::draw_side(app, f, area, card_r, focused),
        Family::Idf => return idf::draw_side(app, f, area, card_r, focused),
        Family::Stm32 => {}
    }
    let dim = Style::new().fg(P.dim);
    if app.prog.cli.is_none() {
        let note = if app.prog.installing.is_some() {
            Line::styled("Finish installing in the terminal below (or in your browser): this view looks for the tool every few seconds.", Style::new().fg(P.git_modified))
        } else {
            Line::styled("Install runs ST's installer in the terminal below. ST asks you to sign in to download it: without one in ~/Downloads, Install opens its download page.", dim)
        };
        let lines = vec![
            Line::styled("STM32CubeProgrammer is not installed.", Style::new().bold()),
            Line::raw(""),
            Line::raw(format!("termcode programs and reads STM32 chips through an ST-LINK with its command line tool, {CLI}.")),
            Line::raw(""),
            note,
        ];
        let p = Paragraph::new(lines).wrap(Wrap { trim: true }).style(Style::new().fg(P.sidebar_fg));
        let h = p.line_count(area.width) as u16;
        f.render_widget(p, area);
        let y = area.y + h + 1;
        let x = button(app, f, area, (area.x, y), glyph(I_SAVE), "Install", Look::Primary, true, Hit::Install, P.sidebar_bg);
        button(app, f, area, (x, y), glyph(I_REFRESH), "Check Again", Look::Secondary, true, Hit::Recheck, P.sidebar_bg);
        return;
    }
    // The target, as a header: the chip, its name, the connection.
    let s = &app.prog;
    let mut y = area.y;
    let name = info(s, "Device name").map_or("No target".to_string(), str::to_string);
    text_at(f, area, (area.x, y), vec![Span::styled(fit(&name, area.width as usize), Style::new().fg(P.sidebar_fg).bold())]);
    y += 1;
    let (st, c) = if s.connected { ("● Connected", P.git_added) } else { ("○ Not connected", P.dim) };
    text_at(f, area, (area.x, y), vec![Span::styled(st, Style::new().fg(c))]);
    y += 2;
    let running = s.job.as_ref().filter(|j| j.op.family() == Family::Stm32).map(|j| feature_of(j.op));
    let serial_on = s.serial.is_some();
    let rows: Vec<(&str, &str, Option<ratatui::style::Color>)> = FEATURES
        .iter()
        .map(|(feat, name, _)| {
            // What runs: the job's row, the console's.
            let dot = if running == Some(*feat) { Some(P.git_modified) } else { (*feat == Feature::Serial && serial_on).then_some(P.git_added) };
            (feature_icon(*feat), *name, dot)
        })
        .collect();
    y = feature_rows(app, f, area, card_r, focused, y, &rows, app.prog.feature, Hit::Feature) + 1;
    let probe = probe_text(&app.prog);
    let home = std::env::var("HOME").unwrap_or_default();
    let tool = app.prog.cli.as_ref().map_or(String::new(), |c| c.display().to_string());
    let tool = match tool.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => tool,
    };
    let mut lines = vec![Line::styled("PROBE", dim.bold()), Line::styled(probe, Style::new().fg(P.sidebar_fg))];
    if let Some(p) = &app.prog.serial {
        lines.extend([Line::raw(""), Line::styled("SERIAL", dim.bold()), Line::styled(format!("● {} at {} baud", tty_name(&p.path), p.baud), Style::new().fg(P.git_added))]);
    }
    lines.extend([Line::raw(""), Line::styled("TOOL", dim.bold()), Line::styled(tool, dim)]);
    if y < area.bottom() {
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), Rect::new(area.x, y, area.width, area.bottom() - y));
    }
}

/// The features, a row each (its icon, its name, a dot while it runs), the chosen one lit.
/// Returns the row after them.
#[allow(clippy::too_many_arguments)]
fn feature_rows(app: &mut App, f: &mut Frame, area: Rect, card_r: Rect, focused: bool, mut y: u16, rows: &[(&str, &str, Option<ratatui::style::Color>)], chosen: usize, hit: fn(usize) -> Hit) -> u16 {
    for (i, (icon, name, dot)) in rows.iter().enumerate() {
        if y >= area.bottom() {
            break;
        }
        let rr = Rect::new(card_r.x, y, card_r.width, 1);
        let sel = i == chosen;
        let hot = app.hover.is_some_and(|p| rr.contains(p));
        let bg = match (sel, focused, hot) {
            (true, true, _) => P.list_active_bg,
            (true, false, _) => P.list_inactive_bg,
            (false, _, true) => P.list_hover_bg,
            _ => P.sidebar_bg,
        };
        fill(f, rr, bg);
        if sel {
            f.render_widget(Span::styled("▎", Style::new().fg(P.focus_border).bg(bg)), Rect::new(card_r.x, y, 1, 1));
        }
        let icon_c = if sel { P.focus_border } else { P.icon_fg };
        row(f, Rect::new(area.x, y, area.width, 1), bg, vec![Span::styled(format!("{icon} "), Style::new().fg(icon_c)), Span::styled(name.to_string(), Style::new().fg(P.sidebar_fg))]);
        if let Some(c) = dot.filter(|_| area.width > 4) {
            f.render_widget(Span::styled("●", Style::new().fg(c).bg(bg)), Rect::new(area.right() - 1, y, 1, 1));
        }
        app.prog.hits.push((rr, hit(i)));
        y += 1;
    }
    y
}

/// The page's top: the target (its name, the connection, Connect or Disconnect) and what
/// it is.
fn target_bar(app: &mut App, f: &mut Frame, r: Rect) {
    let inner = card(f, r, &format!("{} STM32", icons().chip), P.widget_border);
    if inner.height == 0 {
        return;
    }
    let s = &app.prog;
    let on = s.job.is_none();
    let connected = s.connected;
    let name = info(s, "Device name").map(str::to_string);
    let details: Vec<String> = ["Board", "Device CPU", "NVM size", "Flash size", "Voltage", "SWD freq", "JTAG freq"].iter().filter_map(|k| info(s, k).map(|v| v.to_string())).collect();
    let details = if details.is_empty() { probe_text(s) } else { details.join(" · ") };
    // The right end: the state, then the button.
    let (icon, label, hit) = if connected { (glyph(I_DISCONNECT), "Disconnect", Hit::Disconnect) } else { (glyph(I_CONNECT), "Connect", Hit::Run(Op::Connect)) };
    let bw = format!("{icon} {label}").width() as u16 + 2;
    let bx = inner.right().saturating_sub(bw);
    let (st, c) = if connected { ("● Connected", P.git_added) } else { ("○ Not connected", P.dim) };
    let sx = bx.saturating_sub(st.width() as u16 + 3);
    text_at(f, inner, (sx, inner.y), vec![Span::styled(st, Style::new().fg(c))]);
    button(app, f, inner, (bx, inner.y), icon, label, if connected { Look::Secondary } else { Look::Primary }, on, hit, P.editor_bg);
    let room = sx.saturating_sub(inner.x + 1) as usize;
    let title = match name {
        Some(n) => Span::styled(fit(&n, room), Style::new().fg(P.editor_fg).bold()),
        None => Span::styled(fit("No target connected", room), Style::new().fg(P.dim)),
    };
    text_at(f, inner, (inner.x, inner.y), vec![title]);
    if inner.height > 1 {
        text_at(f, inner, (inner.x, inner.y + 1), vec![Span::styled(fit(&details, inner.width as usize), Style::new().fg(P.dim))]);
    }
}

/// A Program job's steps: each one's name, and whether it is done, running or to come.
fn steps(s: &Programmer, j: &Job) -> Vec<(&'static str, u8)> {
    let mut v: Vec<(&str, &[&str])> = vec![("Erase", &["Erasing", "erase"]), ("Download", &["Download in Progress", "Memory Programming"])];
    if s.verify {
        v.push(("Verify", &["Verifying"]));
    }
    if s.run_after {
        v.push(("Run", &["reset is performed", "MCU Reset"]));
    }
    let at = j.out.iter().filter_map(|l| v.iter().rposition(|(_, keys)| keys.iter().any(|k| l.contains(k)))).max().unwrap_or(0);
    v.iter().enumerate().map(|(i, (n, _))| (*n, if i < at { 2 } else if i == at { 1 } else { 0 })).collect()
}

/// The running job, on the page: its bar and (programming) its steps; else how the last
/// one ended. Returns the rows it took.
fn job_card(app: &mut App, f: &mut Frame, r: Rect) -> u16 {
    let s = &app.prog;
    let ascii = crate::theme::ascii();
    let Some(j) = &s.job else {
        let here = |op: Op| match s.family {
            Family::Arduino => (op == Op::Boards && s.ard.feature == 0) || (matches!(op, Op::Compile | Op::Upload) && s.ard.feature == arduino::SKETCH),
            Family::Idf => op.family() == Family::Idf && s.idf.feature != idf::SERIAL,
            Family::Stm32 => op.family() == Family::Stm32 && feature_of(op) == FEATURES[s.feature].0,
        };
        let Some((ok, text)) = s.last.as_ref().filter(|_| s.last_op.is_some_and(here)) else { return 0 };
        let (mark, c) = if *ok { (if ascii { "ok" } else { "✓" }, P.git_added) } else { (if ascii { "x" } else { "✗" }, P.error_fg) };
        text_at(f, r, (r.x + 1, r.y), vec![Span::styled(format!("{mark} {text}"), Style::new().fg(c))]);
        return 2;
    };
    let t = j.since.elapsed().as_secs();
    let program = j.op == Op::Program;
    let h = if program { 4 } else { 3 };
    let inner = card(f, Rect::new(r.x, r.y, r.width, h.min(r.height)), &format!("{} {} · {}:{:02}", if ascii { ">" } else { "▶" }, j.what, t / 60, t % 60), P.focus_border);
    if inner.height == 0 {
        return h;
    }
    let bar_w = inner.width.saturating_sub(6) as usize;
    let (on, off) = if ascii { ("#", "-") } else { ("━", "─") };
    let bar = match j.pct {
        Some(p) => {
            let n = bar_w * p.min(100) as usize / 100;
            vec![Span::styled(on.repeat(n), Style::new().fg(P.focus_border)), Span::styled(off.repeat(bar_w - n), Style::new().fg(P.widget_border)), Span::styled(format!(" {p:>3}%"), Style::new().fg(P.editor_fg).bold())]
        }
        None => vec![Span::styled("Working…", Style::new().fg(P.dim))],
    };
    text_at(f, inner, (inner.x, inner.y), bar);
    if program && inner.height > 1 {
        let mut spans = Vec::new();
        for (i, (name, state)) in steps(s, j).into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(if ascii { "  >  " } else { "  ›  " }, Style::new().fg(P.dim)));
            }
            let (mark, c) = match state {
                2 => (if ascii { "v" } else { "✓" }, P.git_added),
                1 => (if ascii { "*" } else { "●" }, P.focus_border),
                _ => (if ascii { "o" } else { "○" }, P.dim),
            };
            spans.push(Span::styled(format!("{mark} "), Style::new().fg(c)));
            spans.push(Span::styled(name, Style::new().fg(if state == 0 { P.dim } else { P.editor_fg })));
        }
        text_at(f, inner, (inner.x, inner.y + 1), spans);
    }
    h + 1
}

/// The page: the target, the feature's title, the running job, the feature's cards.
pub fn draw_page(app: &mut App, f: &mut Frame, body: Rect) {
    fill(f, body, P.editor_bg);
    ensure_checked(app);
    app.prog.main_area = Rect::default();
    let b = Rect::new(body.x + 1, body.y, body.width.saturating_sub(2), body.height);
    if b.width < 30 || b.height < 6 {
        return;
    }
    match app.prog.family {
        Family::Arduino => return arduino::draw_page(app, f, b),
        Family::Idf => return idf::draw_page(app, f, b),
        Family::Stm32 => {}
    }
    if app.prog.cli.is_none() {
        text_at(f, b, (b.x + 1, b.y + 1), vec![Span::styled(format!("{CLI} is not installed: the Programmer view in the sidebar installs it."), Style::new().fg(P.dim))]);
        return;
    }
    let mut y = b.y;
    let bar_h = 4.min(b.height);
    target_bar(app, f, Rect::new(b.x, y, b.width, bar_h));
    y += bar_h + 1;
    let (feature, title, about) = FEATURES[app.prog.feature];
    text_at(f, b, (b.x + 1, y), vec![Span::styled(format!("{} ", feature_icon(feature)), Style::new().fg(P.focus_border)), Span::styled(title.to_string(), Style::new().fg(P.editor_fg).bold()), Span::styled(format!("   {about}"), Style::new().fg(P.dim))]);
    y += 2;
    if y < b.bottom() {
        y += job_card(app, f, Rect::new(b.x, y, b.width, b.bottom() - y));
    }
    if y + 3 > b.bottom() {
        return;
    }
    let rest = Rect::new(b.x, y, b.width, b.bottom() - y);
    match feature {
        Feature::Device => page_device(app, f, rest),
        Feature::Memory => page_memory(app, f, rest),
        Feature::Program => page_program(app, f, rest),
        Feature::Erase => page_erase(app, f, rest),
        Feature::OptionBytes => page_ob(app, f, rest),
        Feature::Serial => page_serial(app, f, rest),
    }
}

/// `r`'s top `h` rows, and what is left under them (a row between).
fn take(r: Rect, h: u16) -> (Rect, Rect) {
    let h = h.min(r.height);
    (Rect::new(r.x, r.y, r.width, h), Rect::new(r.x, r.y + h + u16::from(r.height > h), r.width, r.height.saturating_sub(h + 1)))
}

fn page_device(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (conn, r) = take(r, 5);
    let c = card(f, conn, "Connection", P.widget_border);
    let lx = c.x + 9;
    for (i, (name, y)) in [("Port", c.y), ("Mode", c.y + 1), ("Speed", c.y + 2)].into_iter().enumerate() {
        label(f, c, (c.x, y), name);
        let (opts, sel, hit): (Vec<String>, usize, fn(usize) -> Hit) = match i {
            0 => (PORTS.iter().map(|p| p.to_string()).collect(), app.prog.port, Hit::Port),
            1 => (MODES.iter().map(|(m, _)| m.to_string()).collect(), app.prog.mode, Hit::Mode),
            _ => (FREQS.iter().map(|(n, _)| n.to_string()).collect(), app.prog.freq, Hit::Freq),
        };
        segmented(app, f, c, (lx, y), &opts, sel, hit, P.editor_bg);
    }
    // The probes: one a row, the one used marked; Refresh and Reset under them.
    let probes = app.prog.probes.clone().unwrap_or_default();
    let (pc, r) = take(r, probes.len().max(1) as u16 + 2);
    let c = card(f, pc, "Probes", P.widget_border);
    if probes.is_empty() {
        text_at(f, c, (c.x, c.y), vec![Span::styled(probe_text(&app.prog), Style::new().fg(P.dim))]);
    }
    for (i, p) in probes.iter().enumerate() {
        let y = c.y + i as u16;
        if y >= c.bottom() {
            break;
        }
        let used = i == app.prog.probe.min(probes.len() - 1);
        let rr = Rect::new(c.x, y, c.width, 1);
        let hot = app.hover.is_some_and(|q| rr.contains(q));
        let mark = if used { "◉" } else { "○" };
        text_at(f, c, (c.x, y), vec![Span::styled(format!("{mark} "), Style::new().fg(if used { P.focus_border } else { P.dim })), Span::styled(probe_line(p), Style::new().fg(P.editor_fg).add_modifier(if hot { Modifier::UNDERLINED } else { Modifier::empty() }))]);
        app.prog.hits.push((rr, Hit::ProbeRow(i)));
    }
    if r.height == 0 {
        return;
    }
    let (acts, r) = take(r, 1);
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_RESET), "Reset Target", Look::Secondary, on, Hit::Run(Op::Reset), P.editor_bg);
    button(app, f, acts, (x, acts.y), glyph(I_REFRESH), "Refresh Probes", Look::Secondary, on, Hit::Run(Op::Probes), P.editor_bg);
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Target", P.widget_border);
    let s = &app.prog;
    if s.info.is_empty() {
        text_at(f, c, (c.x, c.y), vec![Span::styled("Connect (F5) reads the target: its name, ID, flash size and core.", Style::new().fg(P.dim))]);
        return;
    }
    // Two columns where they fit.
    let kw = s.info.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let vw = s.info.iter().map(|(_, v)| v.width()).max().unwrap_or(0);
    let col = (kw + vw + 6) as u16;
    let cols = if c.width >= col * 2 { 2 } else { 1 };
    let per = s.info.len().div_ceil(cols);
    for (i, (k, v)) in s.info.iter().enumerate() {
        let (cx, cy) = (c.x + (i / per) as u16 * col, c.y + (i % per) as u16);
        text_at(f, c, (cx, cy), vec![Span::styled(format!("{k:<kw$}  "), Style::new().fg(P.dim)), Span::styled(v.clone(), Style::new().fg(P.editor_fg))]);
    }
}

fn page_memory(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 5);
    let c = card(f, top, "Range", P.widget_border);
    if c.height == 0 {
        return;
    }
    let s = &app.prog;
    let (a, sz, n) = (hex(s.addr), hex(s.size), size_text(s.size as usize));
    label(f, c, (c.x, c.y), "Address");
    let x = field(app, f, c, (c.x + 9, c.y), 14, &a, "", Hit::Ask(Field::Addr));
    label(f, c, (x + 1, c.y), "Size");
    let x = field(app, f, c, (x + 6, c.y), 14, &sz, "", Hit::Ask(Field::Size));
    label(f, c, (x + 1, c.y), &n);
    // (A refused value says why on the row under the fields.)
    label(f, c, (c.x, c.y + 2), "Width");
    let widths: Vec<String> = WIDTHS.iter().map(|w| format!("{}-bit", w * 8)).collect();
    segmented(app, f, c, (c.x + 9, c.y + 2), &widths, app.prog.width, Hit::Width, P.editor_bg);
    // The actions at the right end: Read (F5) on the first row, Save As… on the last.
    let x = c.right().saturating_sub(13);
    let after = button(app, f, c, (x, c.y), glyph(I_READ), "Read", Look::Primary, on, Hit::Run(Op::ReadMem), P.editor_bg);
    key_hint(f, c, (after, c.y), "F5");
    if app.prog.mem.is_some() {
        button(app, f, c, (x, c.y + 2), glyph(I_SAVE), "Save As…", Look::Secondary, true, Hit::Ask(Field::SaveAs), P.editor_bg);
    }
    if r.height < 4 {
        return;
    }
    let title = match &app.prog.mem {
        Some((base, b)) => format!("Memory · {} – {} · {}", hex(*base), hex(base.wrapping_add(b.len().saturating_sub(1) as u32)), size_text(b.len())),
        None => "Memory".into(),
    };
    let c = card(f, r, &title, P.widget_border);
    app.prog.main_area = c;
    if app.prog.mem.is_none() {
        text_at(f, c, (c.x, c.y), vec![Span::styled("Read (F5) shows the memory here, as STM32CubeProgrammer does.", Style::new().fg(P.dim))]);
        return;
    }
    draw_grid(app, f, c);
}

fn page_program(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 6);
    let c = card(f, top, "Firmware", P.widget_border);
    if c.height == 0 {
        return;
    }
    let file = app.prog.file.clone();
    let bx = c.right().saturating_sub(12);
    button(app, f, c, (bx, c.y), glyph(I_FOLDER), "Browse…", Look::Secondary, true, Hit::Browse, P.editor_bg);
    let room = bx.saturating_sub(c.x + 3);
    // The file's name (a click browses too) and what it is.
    let nr = Rect::new(c.x, c.y, room, 1);
    match &file {
        Some(p) => {
            let name = p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            text_at(f, c, (c.x, c.y), vec![Span::styled(format!("{} ", glyph(I_FILE)), Style::new().fg(P.focus_border)), Span::styled(fit(&name, room.saturating_sub(2) as usize), Style::new().fg(P.editor_fg).bold())]);
            let m = fs::metadata(p).ok();
            let kind = p.extension().map_or(String::new(), |e| e.to_string_lossy().to_uppercase());
            let what = match &m {
                Some(m) => format!("{} · {} · {kind} · changed {}", tilde(p.parent().unwrap_or(p)), size_text(m.len() as usize), m.modified().map_or(String::new(), ago)),
                None => format!("{}: missing", tilde(p)),
            };
            text_at(f, c, (c.x + 2, c.y + 1), vec![Span::styled(fit(&what, c.width.saturating_sub(2) as usize), Style::new().fg(if m.is_some() { P.dim } else { P.error_fg }))]);
        }
        None => text_at(f, c, (c.x, c.y), vec![Span::styled(fit("No file chosen: Browse… picks one, folder by folder.", room as usize), Style::new().fg(P.dim))]),
    }
    app.prog.hits.push((nr, Hit::Browse));
    label(f, c, (c.x, c.y + 2), "Address");
    let a = hex(app.prog.bin_addr);
    let x = field(app, f, c, (c.x + 9, c.y + 2), 14, &a, "", Hit::Ask(Field::BinAddr));
    let bin = file.as_deref().is_none_or(is_bin);
    label(f, c, (x + 1, c.y + 2), if bin { "where a .bin goes" } else { "not used: this file has its own addresses" });
    if r.height < 3 {
        return;
    }
    let (opts, r) = take(r, 3);
    let c = card(f, opts, "Options", P.widget_border);
    let s = &app.prog;
    let (v, e, rr) = (s.verify, s.full_erase, s.run_after);
    let x = check(app, f, c, (c.x, c.y), v, "Verify", Hit::Verify);
    let x = check(app, f, c, (x, c.y), e, "Erase the full chip first", Hit::FullErase);
    check(app, f, c, (x, c.y), rr, "Reset and run after", Hit::RunAfter);
    if r.height == 0 {
        return;
    }
    let (acts, r) = take(r, 1);
    let ready = on && file.is_some();
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_PROGRAM), "Start Programming", Look::Primary, ready, Hit::Run(Op::Program), P.editor_bg);
    key_hint(f, acts, (x, acts.y), "F5");
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Command", P.widget_border);
    if let Ok((args, _, _)) = build(&app.prog, Op::Program) {
        let p = Paragraph::new(Line::from(vec![Span::styled("$ ", Style::new().fg(P.dim)), Span::styled(shown_cmd(CLI, &args), Style::new().fg(P.editor_fg))])).wrap(Wrap { trim: false });
        f.render_widget(p, c);
    }
}

fn page_erase(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 4);
    let c = card(f, top, "Sectors", P.widget_border);
    if c.height == 0 {
        return;
    }
    label(f, c, (c.x, c.y), "Sectors");
    let s = app.prog.sectors.clone();
    let x = field(app, f, c, (c.x + 9, c.y), 18, &s, "0-3, or 0 1 2", Hit::Ask(Field::Sectors));
    button(app, f, c, (x + 1, c.y), glyph(I_ERASE), "Erase Sectors", Look::Secondary, on, Hit::Confirm(Op::EraseSectors), P.editor_bg);
    if app.prog.field_err.as_ref().is_none_or(|(f, _)| *f != Field::Sectors) {
        label(f, c, (c.x + 9, c.y + 1), "0-3 erases sectors 0 to 3; 0 1 2 those three.");
    }
    if r.height < 4 {
        return;
    }
    let (dz, _) = take(r, 4);
    let c = card(f, dz, "Danger zone", P.error_fg);
    text_at(f, c, (c.x, c.y), vec![Span::styled("Erase the whole flash: every byte on the chip goes, and it can't be undone.", Style::new().fg(P.editor_fg))]);
    button(app, f, c, (c.x, c.y + 1), glyph(I_ERASE), "Erase Full Chip", Look::Danger, on, Hit::Confirm(Op::EraseChip), P.editor_bg);
}

fn page_ob(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (acts, r) = take(r, 1);
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_SHIELD), "Read Option Bytes", Look::Primary, on, Hit::Run(Op::ReadOb), P.editor_bg);
    key_hint(f, acts, (x, acts.y), "F5");
    label(f, acts, (x + 4, acts.y), "Read only: change them in STM32CubeProgrammer.");
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Option bytes", P.widget_border);
    app.prog.main_area = c;
    let s = &app.prog;
    if s.ob.is_empty() {
        text_at(f, c, (c.x, c.y), vec![Span::styled("Read (F5) shows them here.", Style::new().fg(P.dim))]);
        return;
    }
    let (dim, fg) = (Style::new().fg(P.dim), Style::new().fg(P.editor_fg));
    let w = s.ob.iter().map(|o| if let Ob::Field(k, _, _) = o { k.width() } else { 0 }).max().unwrap_or(0);
    let vw = s.ob.iter().map(|o| if let Ob::Field(_, v, _) = o { v.width() } else { 0 }).max().unwrap_or(0);
    let mut lines: Vec<Line> = Vec::new();
    for o in &s.ob {
        match o {
            Ob::Group(g) => {
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::from(vec![Span::styled("▎", Style::new().fg(P.focus_border)), Span::styled(g.clone(), fg.bold())]));
            }
            Ob::Field(k, v, m) => {
                // Read protection says how locked the chip is, in colour.
                let mc = match () {
                    _ if k == "RDP" && m.contains("Level 0") => P.git_added,
                    _ if k == "RDP" && m.contains("Level 1") => P.gh_yellow,
                    _ if k == "RDP" => P.error_fg,
                    _ if m.contains("protection active") => P.gh_yellow,
                    _ => P.dim,
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {k:<w$}  "), dim),
                    Span::styled(format!(" {v:<vw$} "), Style::new().fg(P.input_fg).bg(P.input_bg)),
                    Span::raw("  "),
                    Span::styled(m.clone(), Style::new().fg(mc)),
                ]));
            }
        }
    }
    let view = c.height as usize;
    let most = lines.len().saturating_sub(view);
    app.prog.main_off = app.prog.main_off.min(most);
    let off = app.prog.main_off;
    scrollbar(f, Rect::new(c.right(), c.y, 1, c.height), lines.len(), view, off);
    f.render_widget(Paragraph::new(lines.into_iter().skip(off).collect::<Vec<_>>()), c);
}

fn page_serial(app: &mut App, f: &mut Frame, r: Rect) {
    let (top, r) = take(r, 5);
    let c = card(f, top, "Port", P.widget_border);
    if c.height == 0 {
        return;
    }
    let open = app.prog.serial.is_some();
    // The buttons at the right end, the port and rate left of them.
    let rw = 34;
    let bx = c.right().saturating_sub(rw);
    let (icon, name) = if open { (glyph(I_DISCONNECT), "Disconnect") } else { (glyph(I_CONNECT), "Connect") };
    let x = button(app, f, c, (bx, c.y), icon, name, if open { Look::Secondary } else { Look::Primary }, true, Hit::SerialToggle, P.editor_bg);
    key_hint(f, c, (x, c.y), "F5");
    let x = button(app, f, c, (bx, c.y + 1), glyph(I_SEND), "Send…", Look::Secondary, open, Hit::Ask(Field::Send), P.editor_bg);
    button(app, f, c, (x, c.y + 1), glyph(I_ERASE), "Clear", Look::Secondary, !app.prog.serial_out.is_empty(), Hit::SerialClear, P.editor_bg);
    let tty = app.prog.tty.clone().or_else(|| default_tty(&app.prog));
    let what = tty.as_ref().map_or(String::new(), |p| {
        let name = crate::serial::ports().into_iter().find(|(q, _)| q == p).map_or_else(|| tty_name(p), |(_, w)| w);
        format!("{name}  {}", icons().chev_down)
    });
    label(f, c, (c.x, c.y), "Port");
    let w = bx.saturating_sub(c.x + 11).max(10);
    field(app, f, c, (c.x + 9, c.y), w, &what, "No serial port found: plug the board in", Hit::Tty);
    label(f, c, (c.x, c.y + 1), "Baud");
    let baud = app.prog.baud.to_string();
    let x = field(app, f, c, (c.x + 9, c.y + 1), 12, &baud, "", Hit::Ask(Field::Baud));
    if app.prog.field_err.as_ref().is_none_or(|(f, _)| *f != Field::Baud) {
        let (st, col) = match &app.prog.serial {
            Some(p) => (format!("● {} at {} baud", tty_name(&p.path), p.baud), P.git_added),
            None => ("○ Not connected".to_string(), P.dim),
        };
        text_at(f, c, (x + 1, c.y + 1), vec![Span::styled(fit(&st, bx.saturating_sub(x + 2) as usize), Style::new().fg(col))]);
    }
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Monitor", P.widget_border);
    app.prog.main_area = c;
    let s = &app.prog;
    if s.serial_out.iter().all(String::is_empty) {
        text_at(f, c, (c.x, c.y), vec![Span::styled("What the target prints on its serial port shows here (and in the panel's Serial tab).", Style::new().fg(P.dim))]);
        return;
    }
    serial_lines(s, f, c);
}

/// The console's lines, the newest at the bottom (scrolled up by `serial_off`).
fn serial_lines(s: &Programmer, f: &mut Frame, r: Rect) {
    let h = r.height as usize;
    // The line still coming shows once it has text.
    let len = s.serial_out.len() - usize::from(s.serial_out.last().is_some_and(String::is_empty));
    let end = len.saturating_sub(s.serial_off);
    for (k, l) in s.serial_out[end.saturating_sub(h)..end].iter().enumerate() {
        let note = l.starts_with("── ") || l.starts_with("-- ");
        let st = Style::new().fg(if note { P.dim } else { P.editor_fg });
        f.render_widget(Span::styled(fit(&plain(l), r.width as usize), st), Rect::new(r.x, r.y + k as u16, r.width, 1));
    }
}

/// A row of Memory's grid: the address, the data in `width`-byte values (little-endian,
/// as the target reads them), the bytes as text.
fn grid_row(addr: u64, chunk: &[u8], width: usize) -> (String, String, String) {
    let cell = |i: usize| match chunk.get(i * width..((i + 1) * width).min(chunk.len())) {
        Some(b) if !b.is_empty() => {
            let v = b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64);
            format!("{:>w$}", format!("{v:0n$X}", n = b.len() * 2), w = width * 2)
        }
        _ => " ".repeat(width * 2),
    };
    let sep = if width == 4 { "  " } else { " " };
    let data = (0..16 / width).map(cell).collect::<Vec<_>>().join(sep);
    let text = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
    (format!("0x{addr:08X}"), data, text)
}

fn grid_header(width: usize) -> String {
    let sep = if width == 4 { "  " } else { " " };
    (0..16 / width).map(|i| format!("{:<w$}", format!("{:X}", i * width), w = width * 2)).collect::<Vec<_>>().join(sep)
}

/// Memory as CubeProgrammer shows it: Address, a column per value, ASCII; the header
/// stays, rows alternate, zero values are dim; a scrollbar at the right.
fn draw_grid(app: &mut App, f: &mut Frame, a: Rect) {
    let rows = app.prog.mem.as_ref().map_or(0, |(_, b)| b.len().div_ceil(16));
    let h = a.height.saturating_sub(1) as usize;
    let off = app.prog.main_off.min(rows.saturating_sub(h));
    app.prog.main_off = off;
    let s = &app.prog;
    let Some((base, bytes)) = &s.mem else { return };
    let w = WIDTHS[s.width];
    let (dim, fg) = (Style::new().fg(P.dim), Style::new().fg(P.editor_fg));
    let bar = if crate::theme::ascii() { "|" } else { "│" };
    let head = Rect::new(a.x, a.y, a.width.saturating_sub(1), 1);
    row(f, head, P.widget_bg, vec![Span::styled(format!("{:<10}   {}  {bar} ASCII", "Address", grid_header(w)), Style::new().fg(P.dim).bold())]);
    for (k, chunk) in bytes.chunks(16).skip(off).take(h).enumerate() {
        let (addr, data, text) = grid_row(*base as u64 + ((off + k) * 16) as u64, chunk, w);
        let bg = if (off + k) % 2 == 1 { P.cur_line } else { P.editor_bg };
        let mut spans = vec![Span::styled(addr, dim), Span::raw("   ")];
        // Each value: dim when it is zero.
        for (i, cell) in data.split(if w == 4 { "  " } else { " " }).enumerate() {
            if i > 0 {
                spans.push(Span::raw(if w == 4 { "  " } else { " " }));
            }
            let zero = cell.trim().chars().all(|c| c == '0');
            spans.push(Span::styled(cell.to_string(), if zero { dim } else { fg }));
        }
        spans.push(Span::styled(format!("  {bar} "), Style::new().fg(P.widget_border)));
        spans.extend(text.chars().map(|c| Span::styled(c.to_string(), if c == '.' { dim } else { fg })));
        row(f, Rect::new(a.x, a.y + 1 + k as u16, a.width.saturating_sub(1), 1), bg, spans);
    }
    scrollbar(f, Rect::new(a.right().saturating_sub(1), a.y + 1, 1, h as u16), rows, h, off);
}

/// The panel's tabs on its top border: Terminal, Programmer, and Serial once the console
/// was used. Returns where the terminals' own tabs go.
pub fn panel_tabs(app: &mut App, f: &mut Frame, r: Rect) -> u16 {
    let mut x = r.x + 2;
    let s = &app.prog;
    let shown_now = if !s.log_shown { 0 } else if s.serial_tab { 2 } else { 1 };
    let mut tabs = vec![(" Terminal ", Hit::PanelTerm), (" Programmer ", Hit::PanelLog)];
    if s.has_serial() {
        tabs.push((" Serial ", Hit::PanelSerial));
    }
    for (i, (name, hit)) in tabs.into_iter().enumerate() {
        let w = (name.width() as u16).min(r.right().saturating_sub(x + 1));
        if w == 0 {
            break;
        }
        let shown = i == shown_now;
        let st = if shown { Style::new().fg(P.editor_fg).bold() } else { Style::new().fg(P.dim) };
        let tr = Rect::new(x, r.y, w, 1);
        let word = fit(name.trim(), w.saturating_sub(2) as usize);
        let line = Line::from(vec![Span::styled(" ", st), Span::styled(word, if shown { st.underlined() } else { st }), Span::styled(" ", st)]);
        f.render_widget(line, tr);
        app.prog.hits.push((tr, hit));
        x += w + 1;
    }
    x
}

/// The Serial tab: the port's state, then what the target printed.
fn draw_serial(app: &mut App, f: &mut Frame, r: Rect) {
    let s = &app.prog;
    let status = match &s.serial {
        Some(p) => Span::styled(format!(" ● {} at {} baud", tty_name(&p.path), p.baud), Style::new().fg(P.git_added)),
        None => Span::styled(" ○ Not connected (Serial Console connects)", Style::new().fg(P.dim)),
    };
    row(f, Rect::new(r.x, r.y, r.width, 1), P.editor_bg, vec![status]);
    serial_lines(s, f, Rect::new(r.x + 1, r.y + 1, r.width.saturating_sub(1), r.height.saturating_sub(1)));
}

/// The Programmer tab: the job's progress (or how the last one ended), then the log; or
/// the Serial tab.
pub fn draw_log(app: &mut App, f: &mut Frame, r: Rect) {
    fill(f, r, P.editor_bg);
    if r.height == 0 || r.width < 10 {
        return;
    }
    if app.prog.serial_tab {
        return draw_serial(app, f, r);
    }
    let s = &app.prog;
    let ascii = crate::theme::ascii();
    let status: Vec<Span<'static>> = match (&s.job, &s.last) {
        (Some(j), _) => {
            let t = j.since.elapsed().as_secs();
            let mut v = vec![Span::styled(format!(" {} {}  ", if ascii { ">" } else { "▶" }, j.what), Style::new().fg(P.editor_fg).bold())];
            if let Some(p) = j.pct {
                let n = 30 * p.min(100) as usize / 100;
                let (on, off) = if ascii { ("#", "-") } else { ("━", "─") };
                v.push(Span::styled(on.repeat(n), Style::new().fg(P.focus_border)));
                v.push(Span::styled(off.repeat(30 - n), Style::new().fg(P.dim)));
                v.push(Span::styled(format!(" {p:>3}%"), Style::new().fg(P.editor_fg)));
            }
            v.push(Span::styled(format!("  {}:{:02}", t / 60, t % 60), Style::new().fg(P.dim)));
            v
        }
        (None, Some((ok, text))) => {
            let (mark, c) = if *ok { (if ascii { "ok" } else { "✓" }, P.git_added) } else { (if ascii { "x" } else { "✗" }, P.error_fg) };
            vec![Span::styled(format!(" {mark} {text}"), Style::new().fg(c))]
        }
        (None, None) => vec![Span::styled(" No job has run yet.", Style::new().fg(P.dim))],
    };
    row(f, Rect::new(r.x, r.y, r.width, 1), P.editor_bg, status);
    let h = r.height.saturating_sub(1) as usize;
    let end = s.log.len().saturating_sub(s.log_off);
    for (k, l) in s.log[end.saturating_sub(h)..end].iter().enumerate() {
        let c = if l.starts_with("$ ") {
            P.focus_border
        } else if l.trim_start().to_lowercase().starts_with("error") {
            P.error_fg
        } else if l.contains("successfully") || l.contains("complete") {
            P.git_added
        } else {
            P.editor_fg
        };
        let y = r.y + 1 + k as u16;
        f.render_widget(Span::styled(format!(" {}", fit(l, r.width.saturating_sub(1) as usize)), Style::new().fg(c)), Rect::new(r.x, y, r.width, 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Side;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;

    #[test]
    fn the_tools_output_reads_as_text_progress_and_fields() {
        assert_eq!(plain("\x1b[39;49m\x1b[0m\x1b[00;32m[=====     ] 50% \x1b[0m"), "[=====     ] 50%");
        assert_eq!(progress("[==================================================] 100%"), Some(100));
        assert_eq!(progress("[x] 50 cats"), None);
        assert_eq!(progress("Read progress:"), None);
        let out: Vec<String> = ["ST-LINK FW  : V2J45M31", "Board       : NUCLEO-F767ZI", "Device ID   : 0x451", "Device name : STM32F76x/STM32F77x", "NVM size    : 2 MBytes", "Device ID   : 0x999", "Size          : 16384 Bytes"].map(String::from).to_vec();
        let info = parse_info(&out);
        assert_eq!(info.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["ST-LINK FW", "Board", "Device ID", "Device name", "NVM size"], "known keys, first value, in order");
        assert_eq!(info[3].1, "STM32F76x/STM32F77x");
        let out: Vec<String> = ["-------- Connected ST-LINK Probes List --------", "ST-Link Probe 0 :", "   ST-LINK SN  : 0001", "   ST-LINK FW  : V2J45M31", "   Board Name  : NUCLEO-F767ZI", "ST-Link Probe 1 :", "   ST-LINK SN  : 0002", "   ST-LINK FW  : V3J7"].map(String::from).to_vec();
        assert_eq!(parse_probes(&out), vec![Probe { sn: "0001".into(), fw: "V2J45M31".into(), board: "NUCLEO-F767ZI".into() }, Probe { sn: "0002".into(), fw: "V3J7".into(), board: String::new() }]);
        let out: Vec<String> = ["Size          : 8 Bytes", "OPTION BYTES BANK: 0", "   Read Out Protection:", "     RDP          : 0xAA (Level 0, no protection) ", "   Boot address Option Bytes:", "     BOOT_ADD0    : 0x80  (0x200000) ", "     nWRP0        : 0x1"].map(String::from).to_vec();
        assert_eq!(
            parse_ob(&out),
            vec![
                Ob::Group("Bank 0".into()),
                Ob::Group("Read Out Protection".into()),
                Ob::Field("RDP".into(), "0xAA".into(), "Level 0, no protection".into()),
                Ob::Group("Boot address Option Bytes".into()),
                Ob::Field("BOOT_ADD0".into(), "0x80".into(), "0x200000".into()),
                Ob::Field("nWRP0".into(), "0x1".into(), String::new()),
            ]
        );
        assert_eq!(elapsed(&["Time elapsed during read operation is: 00:00:00.107".to_string()]).as_deref(), Some("0.1 s"));
        assert_eq!(elapsed(&["Time elapsed during download operation: 00:02:05.000".to_string()]).as_deref(), Some("2:05"));
    }

    #[test]
    fn typed_values_become_the_tools_arguments() {
        assert_eq!((num("0x08000000"), num("1024"), num("16K"), num("2m"), num("0x"), num("zz")), (Some(0x0800_0000), Some(1024), Some(16384), Some(2 << 20), None, None));
        assert_eq!(sector_args("0-3"), Some(vec!["[0".to_string(), "3]".to_string()]));
        assert_eq!(sector_args("0, 1 2"), Some(vec!["0".to_string(), "1".into(), "2".into()]));
        assert_eq!((sector_args("3-1"), sector_args(""), sector_args("a")), (None, None, None));
        let mut s = Programmer { file: Some("/w/build/fw.bin".into()), mode: 2, ..Default::default() };
        let (args, what, _) = build(&s, Op::Program).unwrap();
        assert_eq!(args.join(" "), "-c port=SWD mode=HOTPLUG -d /w/build/fw.bin 0x08000000 -v -rst");
        assert_eq!(what, "Programming fw.bin");
        (s.file, s.full_erase, s.verify, s.run_after, s.port) = (Some("/w/fw.elf".into()), true, false, false, 1);
        s.probes = Some(vec![Probe { sn: "A".into(), ..Default::default() }, Probe { sn: "B".into(), ..Default::default() }]);
        s.probe = 1;
        assert_eq!(build(&s, Op::Program).unwrap().0.join(" "), "-c port=JTAG mode=HOTPLUG sn=B -e all -d /w/fw.elf", "an .elf has its own address; the chosen probe of two");
        assert_eq!(build(&Programmer::default(), Op::Program).unwrap_err(), "Choose a file to program first");
        assert_eq!(build(&Programmer::default(), Op::Probes).unwrap().0.join(" "), "-l stlink-only", "listing touches no target");
        assert_eq!(shown_cmd(CLI, &["-d".into(), "/a b/fw.hex".into()]), "STM32_Programmer_CLI -d '/a b/fw.hex'");
        let root = Path::new("/w");
        assert_eq!(typed_path(root, " ~/fw/a.bin "), home().join("fw/a.bin"));
        assert_eq!(typed_path(root, "'/a b/c.bin'"), PathBuf::from("/a b/c.bin"), "a pasted, quoted path");
        assert_eq!(typed_path(root, "file:///x/y.hex"), PathBuf::from("/x/y.hex"));
        assert_eq!((typed_path(root, "build/z.elf"), typed_path(root, "~x.bin")), (PathBuf::from("/w/build/z.elf"), PathBuf::from("/w/~x.bin")));
    }

    /// Prints what STM32_Programmer_CLI prints, in its colours, for the arguments used.
    fn fake_cli(dir: &Path) -> PathBuf {
        let bin = dir.join(CLI);
        let script = concat!(
            "#!/bin/sh\n",
            "d=$(dirname \"$0\")\n",
            "echo \"$*\" >> \"$d/cli.log\"\n",
            "printf '\\033[36m      -------------------\\n        STM32CubeProgrammer v2.23.0\\n      -------------------\\033[0m\\n\\n'\n",
            "case \"$*\" in *stlink-only*) printf 'ST-Link Probe 0 :\\n   ST-LINK SN  : 0001TEST\\n   ST-LINK FW  : V2J45M31\\n   Board Name  : NUCLEO-TEST\\n'; exit 0 ;; esac\n",
            "case \"$*\" in *fail*) printf '\\033[31mError: No STM32 target found!\\033[0m\\n'; exit 1 ;; esac\n",
            "printf 'ST-LINK FW  : V2J45M31\\nBoard       : NUCLEO-TEST\\nDevice ID   : 0x451\\nDevice name : STM32F76x/STM32F77x\\nNVM size    : 2 MBytes\\nDevice CPU  : Cortex-M7\\n'\n",
            "case \"$*\" in\n",
            "  *' -u '*) for a; do last=$a; done; printf 'ABCDEFGHIJKLMNOPQR' > \"$last\"; printf 'Read progress:\\n\\033[32m[=====     ] 50%% \\r[==========] 100%% \\n\\nData read successfully\\nTime elapsed during read operation is: 00:00:00.107\\n' ;;\n",
            "  *' -d '*) printf 'Download in Progress:\\n[====      ] 40%% \\r[==========] 100%% \\n\\nFile download complete\\nTime elapsed during download operation: 00:00:01.250\\n'; case \"$*\" in *' -v'*) printf 'Download verified successfully \\n' ;; esac ;;\n",
            "  *'-e all'*) printf 'Mass erase ...\\nMass erase successfully achieved\\n' ;;\n",
            "  *'-ob displ'*) printf 'OPTION BYTES BANK: 0\\n   Read Out Protection:\\n     RDP          : 0xAA (Level 0, no protection) \\n   BOR Level:\\n     BOR_LEV      : 0x0 (BOR Level 3) \\n' ;;\n",
            "esac\n",
        );
        fs::write(&bin, script).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn settle(app: &mut App) {
        for _ in 0..400 {
            tick(app);
            if app.prog.job.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the job never ended");
    }

    fn screen(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// A click on the button (or row) showing `label`, as drawn.
    fn press(app: &mut App, t: &mut Terminal<TestBackend>, label: &str) {
        let s = screen(app, t);
        // A button's text is its icon and label between round caps.
        let hit = app.prog.hits.iter().find(|(r, _)| {
            let line = s.lines().nth(r.y as usize).unwrap_or("");
            let text: String = line.chars().skip(r.x as usize).take(r.width as usize).collect();
            let t = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{e0b6}' || c == '\u{e0b4}');
            t == label.trim() || t.ends_with(&format!(" {}", label.trim()))
        });
        let r = hit.unwrap_or_else(|| panic!("no {label:?}\n{s}")).0;
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            app.on_mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE });
        }
    }

    fn setup(tag: &str) -> (App, Terminal<TestBackend>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("termcode-stm32-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("build")).unwrap();
        fs::create_dir_all(dir.join("tool")).unwrap();
        let mut app = App::new(dir.clone(), None);
        app.prog.cli = Some(fake_cli(&dir.join("tool")));
        app.prog.checked = true;
        app.side_w = Some(36);
        (app, Terminal::new(TestBackend::new(160, 64)).unwrap(), dir)
    }

    #[test]
    fn device_page_lists_the_probe_connects_and_shows_the_log_below() {
        let (mut app, mut t, dir) = setup("device");
        app.show_side(Side::Programmer, false);
        settle(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("PROGRAMMER") && s.contains("Option Bytes") && s.contains("NUCLEO-TEST · ST-LINK V2J45M31 · SN 0001TEST"), "{s}");
        assert!(s.contains(" Terminal ") && s.contains(" Programmer ") && s.contains("✓ Found an ST-LINK probe"), "the log under the page\n{s}");
        assert!(s.contains("$ STM32_Programmer_CLI -l stlink-only"), "{s}");
        press(&mut app, &mut t, "Hot plug");
        press(&mut app, &mut t, "Connect");
        settle(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Device name") && s.contains("STM32F76x/STM32F77x") && s.contains("Cortex-M7"), "{s}");
        assert!(s.contains("✓ Connecting: done"), "{s}");
        let log = fs::read_to_string(dir.join("tool/cli.log")).unwrap();
        assert!(log.lines().last() == Some("-c port=SWD mode=HOTPLUG"), "{log}");
        assert!(s.contains("● Connected") && s.contains(" Disconnect"), "{s}");
        press(&mut app, &mut t, "Disconnect");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("○ Not connected") && s.contains(" Connect") && s.contains("reads the target") && app.prog.info.is_empty(), "the target forgotten\n{s}");
        // The Terminal tab: a shell's; the Programmer tab back.
        press(&mut app, &mut t, "Terminal");
        assert!(!app.prog.log_shown && app.focus == Focus::Term && !app.terms.is_empty());
        press(&mut app, &mut t, "Programmer");
        assert!(app.prog.log_shown && app.focus != Focus::Term);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn program_shows_progress_then_the_result_and_erase_asks_first() {
        let (mut app, mut t, dir) = setup("program");
        fs::write(dir.join("build/fw.bin"), [0u8; 64]).unwrap();
        app.show_side(Side::Programmer, false);
        settle(&mut app);
        press(&mut app, &mut t, "Program");
        assert!(screen(&mut app, &mut t).contains("No file chosen"), "no file yet");
        // The browser (here from the workspace, not the home folder): into build/, fw.bin.
        app.prog.browsed = Some(dir.clone());
        press(&mut app, &mut t, "Browse…");
        let labels = |app: &App| app.gitui.cmds.picker.as_ref().unwrap().items.iter().map(|i| i.label.clone()).collect::<Vec<_>>();
        assert_eq!(labels(&app), ["Type a path…", ".. (up)", "build/", "tool/"], "folders to go into");
        crate::gitcmds::pick(&mut app, 2);
        assert_eq!(labels(&app), ["Type a path…", ".. (up)", "The workspace", "fw.bin"]);
        assert_eq!(app.gitui.cmds.picker.as_ref().unwrap().sel, 3, "the first file ready for Enter");
        crate::gitcmds::pick(&mut app, 3);
        assert_eq!(app.prog.file.as_deref(), Some(dir.join("build/fw.bin").as_path()));
        let s = screen(&mut app, &mut t);
        assert!(s.contains("fw.bin") && s.contains("Command") && s.contains("$ STM32_Programmer_CLI -c port=SWD mode=NORMAL -d") && s.contains("-v -rst"), "the command it runs\n{s}");
        press(&mut app, &mut t, "Start Programming");
        assert!(screen(&mut app, &mut t).contains("▶ Programming fw.bin"), "running");
        settle(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("✓ Programming fw.bin: done in 1.2 s, verified") && s.contains("Download verified successfully"), "{s}");
        let log = fs::read_to_string(dir.join("tool/cli.log")).unwrap();
        assert!(log.lines().last().unwrap().ends_with("/build/fw.bin 0x08000000 -v -rst"), "{log}");
        // A typed file must be one the tool takes.
        fs::write(dir.join("notes.txt"), "x").unwrap();
        assert!(entered(&mut app, Field::File, "notes.txt").is_err() && entered(&mut app, Field::File, "nope.bin").is_err());
        // Erase asks first; Cancel runs nothing.
        press(&mut app, &mut t, "Erase");
        press(&mut app, &mut t, "Erase Full Chip");
        assert!(app.prompt.is_some());
        let runs = app.prog.runs;
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!((app.prompt.is_none(), app.prog.runs), (true, runs));
        press(&mut app, &mut t, "Erase Full Chip");
        app.run_prompt(PAct::Prog(Op::EraseChip));
        settle(&mut app);
        assert!(screen(&mut app, &mut t).contains("✓ Erasing the full chip: done"));
        // The page's × : back to the code (the Explorer, no Programmer panel).
        let (g, i) = (app.ag, app.groups[app.ag].tabs.iter().position(|t| t.key == KEY).unwrap());
        app.close_tab(g, i, false);
        let s = screen(&mut app, &mut t);
        assert!(app.side == Side::Explorer && !app.prog.log_shown && !app.term_open && s.contains("EXPLORER") && !s.contains("STM32 PROGRAMMER"), "{s}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn memory_reads_into_a_hex_dump_and_option_bytes_into_a_table() {
        let (mut app, mut t, dir) = setup("memory");
        app.show_side(Side::Programmer, false);
        settle(&mut app);
        press(&mut app, &mut t, "Memory");
        let typed = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
        };
        let key = |app: &mut App, k: KeyCode| app.on_key(KeyEvent::new(k, KeyModifiers::NONE));
        // Size typed in place: it starts empty; a bad value says why in red and stays.
        press(&mut app, &mut t, "0x00000400");
        assert!(!screen(&mut app, &mut t).contains("0x00000400"), "the old value cleared while typing");
        typed(&mut app, "1G");
        key(&mut app, KeyCode::Enter);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Type a size from 1 byte to 4 MB") && s.contains("▏1G"), "{s}");
        let (x, y) = s.lines().enumerate().find_map(|(y, l)| l.find("Type a size").map(|x| (l[..x].chars().count() as u16, y as u16))).unwrap();
        assert_eq!(t.backend().buffer()[(x, y)].fg, P.error_fg);
        // A click elsewhere with a bad value does nothing but keep it; Esc keeps the old one.
        press(&mut app, &mut t, "Read");
        assert!(app.prog.job.is_none() && app.prog.editing.is_some());
        key(&mut app, KeyCode::Esc);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("0x00000400") && !s.contains("Type a size"), "{s}");
        // Enter with a good value reads there.
        press(&mut app, &mut t, "0x00000400");
        app.paste_text("0x12\n");
        key(&mut app, KeyCode::Enter);
        assert!(app.prog.job.is_some() && app.prog.size == 0x12, "Enter reads the new range");
        settle(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Address      0         4         8         C") && s.contains("│ ASCII"), "{s}");
        assert!(s.contains("0x08000000   44434241  48474645  4C4B4A49  504F4E4D  │ ABCDEFGHIJKLMNOP") && s.contains("0x08000010       5251"), "32-bit values as the target reads them\n{s}");
        press(&mut app, &mut t, "8-bit");
        assert!(screen(&mut app, &mut t).contains("0x08000000   41 42 43 44 45 46 47 48 49 4A 4B 4C 4D 4E 4F 50  │ ABCDEFGHIJKLMNOP"));
        assert!(s.contains("✓ Reading 18 bytes at 0x08000000: done in 0.1 s"), "{s}");
        assert!(!std::env::temp_dir().join(format!("termcode-stm32-{}-{}.bin", std::process::id(), app.prog.runs)).exists(), "the upload's file is gone");
        entered(&mut app, Field::SaveAs, "dump.bin").unwrap();
        assert_eq!(fs::read(dir.join("dump.bin")).unwrap(), b"ABCDEFGHIJKLMNOPQR");
        assert!(entered(&mut app, Field::SaveAs, "dump.bin").is_err(), "never over a file");
        press(&mut app, &mut t, "Option Bytes");
        press(&mut app, &mut t, "Read Option Bytes");
        settle(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Read Out Protection") && s.contains("RDP") && s.contains("0xAA") && s.contains("Level 0, no protection"), "{s}");
        // A failing job says why.
        app.prog.file = Some(dir.join("fail.bin"));
        run(&mut app, Op::Program);
        settle(&mut app);
        assert!(screen(&mut app, &mut t).contains("✗ Programming fail.bin failed: No STM32 target found!"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn memory_rows_read_as_the_target_does() {
        let b = b"ABCDEFGHIJKLMNOPQR";
        assert_eq!(grid_row(0x0800_0000, &b[..16], 4).1, "44434241  48474645  4C4B4A49  504F4E4D");
        assert_eq!(grid_row(0x0800_0010, &b[16..], 4), ("0x08000010".into(), format!("    5251{}", " ".repeat(30)), "QR".into()));
        assert_eq!(grid_row(0, &b[..4], 2).1.trim_end(), "4241 4443");
        assert_eq!(grid_header(4), "0         4         8         C       ");
        assert_eq!(grid_header(1).split_whitespace().last(), Some("F"));
    }

    #[test]
    fn the_serial_console_shows_the_targets_output_and_sends_lines() {
        let (mut app, mut t, dir) = setup("serial");
        let (mut m, mut sl) = (0, 0);
        // SAFETY: valid out pointers; name, termios and winsize may be null.
        assert_eq!(unsafe { libc::openpty(&mut m, &mut sl, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
        // SAFETY: openpty opened both; each File owns one.
        let (mut board, _slave) = unsafe { (<fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(m), <fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(sl)) };
        app.prog.tty = Some(fs::read_link(format!("/proc/self/fd/{sl}")).unwrap());
        app.show_side(Side::Programmer, false);
        settle(&mut app);
        press(&mut app, &mut t, "Serial Console");
        assert!(entered(&mut app, Field::Baud, "12345").is_err());
        entered(&mut app, Field::Baud, "921600").unwrap();
        app.on_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)); // the page's main action
        assert!(app.prog.serial.is_some() && app.prog.serial_tab);
        use std::io::Write as _;
        board.write_all(b"\x1b[32mboot ok\x1b[0m\r\ntemp=21").unwrap();
        let shown = |app: &mut App, t: &mut Terminal<TestBackend>, what: &str| {
            for _ in 0..200 {
                tick(app);
                if screen(app, t).contains(what) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("no {what:?}\n{}", screen(app, t));
        };
        shown(&mut app, &mut t, "temp=21");
        let s = screen(&mut app, &mut t);
        assert!(s.contains(" Serial ") && s.contains(" boot ok") && s.contains("at 921600 baud") && !s.contains("[32m"), "colours dropped, the coming line shown\n{s}");
        entered(&mut app, Field::Send, "led on").unwrap();
        assert!(app.gitui.cmds.picker.is_some(), "ready for the next line");
        let mut got = [0u8; 7];
        std::io::Read::read_exact(&mut board, &mut got).unwrap();
        assert_eq!(&got, b"led on\n");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        press(&mut app, &mut t, "Disconnect");
        assert!(app.prog.serial.is_none());
        shown(&mut app, &mut t, "disconnected from");
        assert!(entered(&mut app, Field::Send, "x").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn output_without_newlines_stays_in_bounded_lines() {
        let mut s = Programmer::default();
        serial_data(&mut s, &"x".repeat(10_000));
        assert!(s.serial_out.iter().all(|l| l.len() <= 4096) && s.serial_out.concat().len() == 10_000);
        assert_eq!(progress("[99999999999/1] Linking"), Some(100), "no overflow on a strange count");
    }

    #[test]
    fn an_install_runs_in_a_new_terminal_never_one_in_use() {
        let dir = std::env::temp_dir().join(format!("termcode-install-term-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(dir.clone(), None);
        app.new_terminal();
        let first = app.terms.len();
        // (Never the real install here: its command would run in that shell.)
        assert!(app.run_in_new_terminal(": a harmless no-op"));
        assert_eq!((app.terms.len(), app.term_at), (first + 1, first), "its own terminal");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn arduino_boards_read_from_either_json_shape() {
        let new = r#"{"detected_ports":[{"port":{"address":"/dev/ttyUSB7","protocol":"serial"}},{"port":{"address":"/dev/ttyACM9","protocol":"serial"},"matching_boards":[{"name":"Arduino Uno","fqbn":"arduino:avr:uno"}]}]}"#;
        let old = r#"[{"port":{"address":"COM3","protocol":"serial"},"boards":[{"name":"ESP32 Dev Module","fqbn":"esp32:esp32:esp32"}]}]"#;
        let b = arduino::parse_boards(new);
        assert_eq!((b.len(), b[1].name.as_str(), b[1].fqbn.as_str(), b[0].name.as_str()), (2, "Arduino Uno", "arduino:avr:uno", ""));
        assert_eq!(arduino::parse_boards(old)[0].fqbn, "esp32:esp32:esp32");
        assert!(arduino::parse_boards("not json").is_empty());
        let mut a = arduino::Arduino::default();
        assert_eq!(arduino::boards_came(&mut a, &[new.to_string()]), "Found an Arduino board");
        assert_eq!(a.chosen().map(|b| b.port.as_str()), Some("/dev/ttyACM9"), "boards first, the board chosen");
    }

    #[test]
    fn the_switch_shows_arduino_boards_and_the_console_takes_the_chosen_port() {
        let (mut app, mut t, dir) = setup("arduino");
        let bin = dir.join("tool/arduino-cli");
        let json = r#"{"detected_ports":[{"port":{"address":"/dev/ttyUSB7","protocol":"serial"}},{"port":{"address":"/dev/ttyACM9","protocol":"serial"},"matching_boards":[{"name":"Arduino Uno","fqbn":"arduino:avr:uno"}]}]}"#;
        fs::write(&bin, format!("#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/ard.log\"\nprintf '%s\\n' '{json}'\n")).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        (app.prog.ard.cli, app.prog.ard.checked) = (Some(bin), true);
        app.show_side(Side::Programmer, false);
        settle(&mut app);
        press(&mut app, &mut t, "Arduino");
        settle(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Arduino Uno") && s.contains("arduino:avr:uno") && s.contains("/dev/ttyUSB7") && s.contains("Found an Arduino board"), "{s}");
        assert!(!s.contains("Option Bytes"), "Arduino's own features\n{s}");
        assert_eq!(default_tty(&app.prog), Some(PathBuf::from("/dev/ttyACM9")), "the console opens the board's port");
        // A row chooses: here the bare port.
        let r = app.prog.hits.iter().find(|(_, h)| *h == Hit::Board(1)).unwrap().0;
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            app.on_mouse(MouseEvent { kind, column: r.x + 2, row: r.y, modifiers: KeyModifiers::NONE });
        }
        assert_eq!(app.prog.ard.chosen().map(|b| b.port.as_str()), Some("/dev/ttyUSB7"));
        press(&mut app, &mut t, "Serial Console");
        assert!(screen(&mut app, &mut t).contains("ttyUSB7"), "the chosen port");
        assert!(fs::read_to_string(dir.join("tool/ard.log")).unwrap().contains("board list --format json"));
        assert!(app.prog.log.iter().any(|l| l == "$ arduino-cli board list --format json"), "the log names the tool run");
        // Back to STM32: its pages again.
        press(&mut app, &mut t, "STM32Cube");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Option Bytes") && s.contains("NUCLEO-TEST"), "{s}");
        // No arduino-cli: Arduino's side offers to install it.
        app.prog.ard.cli = None;
        press(&mut app, &mut t, "Arduino");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("arduino-cli is not") && s.contains("Check Again"), "{s}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_and_flash_tools_progress_and_errors_read_as_such() {
        assert_eq!(progress("[123/1024] Building C object esp-idf/main.c.obj"), Some(12));
        assert_eq!(progress("Writing at 0x00010000... (45 %)"), Some(45));
        assert_eq!(progress("Writing | ################################################## | 100% 0.29s"), Some(100));
        assert_eq!(progress("Sketch uses 924 bytes (2%) of program storage space."), None, "a size is no progress");
        assert_eq!(progress("[x] 50%"), Some(50));
        let out: Vec<String> = ["Compiling sketch...", "/w/blink/blink.ino:5:3: error: 'foo' was not declared in this scope", "Error during build: exit status 1"].map(String::from).to_vec();
        assert_eq!(first_error(&out).as_deref(), Some("/w/blink/blink.ino:5:3: error: 'foo' was not declared in this scope"), "the compiler's first");
        assert_eq!(arduino::summary(&["Sketch uses 924 bytes (2%) of program storage space. Maximum is 32256 bytes.".to_string()]).as_deref(), Some("924 bytes (2%) of flash"));
        assert_eq!(idf::summary(&["hello.bin binary size 0x2c5d0 bytes. Smallest app partition is 0x100000 bytes. 0xd3a30 bytes (83%) free.".to_string()]).as_deref(), Some("83% of the app partition free"));
    }

    fn script(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn an_arduino_sketch_compiles_and_uploads_for_the_chosen_board() {
        let (mut app, mut t, dir) = setup("sketch");
        fs::write(dir.join("blink.ino"), "void setup() {}\nvoid loop() {}\n").unwrap();
        let json = r#"{"detected_ports":[{"port":{"address":"/dev/ttyACM9","protocol":"serial"},"matching_boards":[{"name":"Arduino Uno","fqbn":"arduino:avr:uno"}]}]}"#;
        let bin = dir.join("tool/arduino-cli");
        script(&bin, &format!("echo \"$*\" >> \"$(dirname \"$0\")/ard.log\"\ncase \"$1\" in board) printf '%s\\n' '{json}'; exit 0 ;; esac\necho 'Sketch uses 924 bytes (2%) of program storage space. Maximum is 32256 bytes.'\ncase \"$*\" in *--upload*) echo 'Writing | ################################################## | 100% 0.20s' ;; esac\n"));
        (app.prog.ard.cli, app.prog.ard.checked, app.prog.family) = (Some(bin), true, Family::Arduino);
        app.show_side(Side::Programmer, false);
        settle(&mut app);
        press(&mut app, &mut t, "Sketch");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("arduino:avr:uno") && s.contains("/dev/ttyACM9"), "the board's FQBN and port, the workspace the sketch\n{s}");
        // A bad FQBN says why in red; Esc keeps the board's.
        press(&mut app, &mut t, "arduino:avr:uno");
        for c in "uno".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(screen(&mut app, &mut t).contains("Type an FQBN"));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        press(&mut app, &mut t, "Compile");
        settle(&mut app);
        assert!(screen(&mut app, &mut t).contains("Compiling termcode-stm32-sketch-"), "named for the sketch folder");
        let last = app.prog.last.clone().unwrap();
        assert!(last.0 && last.1.ends_with("done · 924 bytes (2%) of flash"), "{last:?}");
        app.on_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE)); // Upload
        settle(&mut app);
        let log = fs::read_to_string(dir.join("tool/ard.log")).unwrap();
        let root = dir.display().to_string();
        assert!(log.contains(&format!("compile -b arduino:avr:uno {root}")) && log.contains(&format!("compile --upload -p /dev/ttyACM9 -b arduino:avr:uno {root}")), "{log}");
        assert!(app.prog.last.as_ref().unwrap().1.starts_with("Uploading"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_esp_idf_project_builds_flashes_around_the_console_and_sets_its_target() {
        let (mut app, mut t, dir) = setup("idf");
        fs::write(dir.join("CMakeLists.txt"), "cmake_minimum_required(VERSION 3.16)\ninclude($ENV{IDF_PATH}/tools/cmake/project.cmake)\nproject(hello)\n").unwrap();
        fs::write(dir.join("sdkconfig"), "CONFIG_IDF_TARGET=\"esp32\"\n").unwrap();
        // A framework: its export.sh puts idf.py on PATH.
        let fw = dir.join("tool/esp-idf");
        fs::create_dir_all(fw.join("bin")).unwrap();
        fs::write(fw.join("export.sh"), "PATH=\"$(dirname \"${BASH_SOURCE[0]}\")/bin:$PATH\"; export PATH\n").unwrap();
        script(&fw.join("bin/idf.py"), concat!(
            "echo \"$*\" >> \"$(dirname \"$0\")/../idf.log\"\n",
            "case \"$*\" in *set-target*) printf 'CONFIG_IDF_TARGET=\"%s\"\\n' \"$4\" > \"$2/sdkconfig\"; exit 0 ;; esac\n",
            "printf '[1/4] Building C object main.c.obj\\n[4/4] Linking\\nProject build complete.\\nhello.bin binary size 0x2c5d0 bytes. Smallest app partition is 0x100000 bytes. 0xd3a30 bytes (83%%) free.\\n'\n",
            "case \"$*\" in *' flash'*) printf 'Writing at 0x00010000... (50 %%)\\nWriting at 0x00020000... (100 %%)\\nHard resetting via RTS pin...\\n' ;; esac\n",
        ));
        // The board: a pseudo-terminal, the console on it.
        let (mut m, mut sl) = (0, 0);
        // SAFETY: valid out pointers; name, termios and winsize may be null.
        assert_eq!(unsafe { libc::openpty(&mut m, &mut sl, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
        // SAFETY: openpty opened both; each File owns one.
        let _board = unsafe { (<fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(m), <fs::File as std::os::unix::io::FromRawFd>::from_raw_fd(sl)) };
        let tty = fs::read_link(format!("/proc/self/fd/{sl}")).unwrap();
        (app.prog.idf.root, app.prog.idf.checked, app.prog.tty) = (Some(fw.clone()), true, Some(tty.clone()));
        app.show_side(Side::Programmer, false);
        settle(&mut app);
        press(&mut app, &mut t, "ESP-IDF");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("hello") && s.contains("esp32") && s.contains("Erase Flash"), "the workspace's project\n{s}");
        press(&mut app, &mut t, "Build");
        settle(&mut app);
        assert!(app.prog.last.as_ref().unwrap().1.ends_with("done · 83% of the app partition free"), "{:?}", app.prog.last);
        // Flash: the console lets go of the port meanwhile, then has it again.
        serial_toggle(&mut app);
        assert!(app.prog.serial.is_some());
        press(&mut app, &mut t, "Flash");
        assert!(app.prog.serial.is_none() && app.prog.job.is_some(), "the port free for the flash");
        settle(&mut app);
        assert!(app.prog.serial.is_some(), "the console back after it");
        let log = fs::read_to_string(fw.join("idf.log")).unwrap();
        assert!(log.contains(&format!("-C {} -p {} flash", dir.display(), tty.display())), "{log}");
        assert!(app.prog.log.iter().any(|l| l.starts_with("$ idf.py -C ")), "the log shows idf.py, not the shell");
        // set-target asks first, then the chip is read again.
        picked(&mut app, Browse::Target("esp32s3".into()));
        assert!(app.prog.job.is_none() && app.prompt.is_some());
        app.run_prompt(PAct::Prog(Op::IdfTarget));
        settle(&mut app);
        assert_eq!(app.prog.idf.target.as_deref(), Some("esp32s3"));
        // No framework: ESP-IDF's side offers to install it.
        app.prog.serial = None;
        app.prog.idf.root = None;
        let s = screen(&mut app, &mut t);
        assert!(s.contains("ESP-IDF is not installed") && s.contains("Check Again"), "{s}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_the_tool_the_view_offers_to_install_it() {
        let (mut app, mut t, dir) = setup("install");
        app.prog.cli = None;
        app.show_side(Side::Programmer, false);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("STM32CubeProgrammer is not") && s.contains(" Install ") && s.contains("Check Again"), "{s}");
        assert!(!app.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == KEY)), "no page");
        command(&mut app, Op::Connect);
        assert!(app.prog.job.is_none() && app.side == Side::Programmer);
        fs::remove_dir_all(&dir).unwrap();
    }
}
