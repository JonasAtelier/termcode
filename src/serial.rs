//! A serial port (a board's UART, an ST-LINK's virtual COM port): opened raw at a baud
//! rate, read on a thread, written to. The STM32 Programmer's console uses it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The rates a port opens at.
pub const BAUDS: [u32; 13] = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000, 2000000];

fn speed(baud: u32) -> Option<libc::speed_t> {
    Some(match baud {
        1200 => libc::B1200,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        230400 => libc::B230400,
        460800 => libc::B460800,
        921600 => libc::B921600,
        1000000 => libc::B1000000,
        2000000 => libc::B2000000,
        _ => return None,
    })
}

/// What the reading thread tells: text came, or the port closed (with why, unless asked).
pub enum Event {
    Data(String),
    End(Option<String>),
}

/// An open port. Dropped, its thread stops (within a tenth of a second) and closes it.
pub struct Port {
    pub path: PathBuf,
    pub baud: u32,
    writer: File,
    stop: Arc<AtomicBool>,
}

impl Port {
    /// `path` raw (8N1, no flow control) at `baud`; what comes in goes to `on`.
    pub fn open(path: &Path, baud: u32, on: Box<dyn Fn(Event) + Send>) -> io::Result<Port> {
        let sp = speed(baud).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{baud} baud is not a rate the port takes")))?;
        // Non-blocking to open (no wait for a carrier), blocking after.
        let f = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK).open(path)?;
        let fd = f.as_raw_fd();
        // SAFETY: fd is open; termios is plain data filled by tcgetattr.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            libc::cfmakeraw(&mut t);
            t.c_cflag |= libc::CLOCAL | libc::CREAD;
            t.c_cflag &= !libc::CRTSCTS;
            // A read waits a tenth of a second at most: the thread sees the stop flag.
            t.c_cc[libc::VMIN] = 0;
            t.c_cc[libc::VTIME] = 1;
            libc::cfsetispeed(&mut t, sp);
            libc::cfsetospeed(&mut t, sp);
            if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
            libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) & !libc::O_NONBLOCK);
        }
        let mut reader = f.try_clone()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, at) = (stop.clone(), path.to_path_buf());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                if flag.load(Ordering::Relaxed) {
                    return on(Event::End(None));
                }
                match reader.read(&mut buf) {
                    Ok(0) if !at.exists() => return on(Event::End(Some("the port is gone (unplugged?)".into()))),
                    Ok(0) => {}
                    Ok(n) => on(Event::Data(String::from_utf8_lossy(&buf[..n]).into_owned())),
                    Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock) => {}
                    Err(e) => return on(Event::End(Some(e.to_string()))),
                }
            }
        });
        Ok(Port { path: path.to_path_buf(), baud, writer: f, stop })
    }

    pub fn send(&mut self, text: &str) -> io::Result<()> {
        self.writer.write_all(text.as_bytes())
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A /dev/serial/by-id name as people read it: "STMicroelectronics STM32 STLink 066B…".
fn nice(id: &str) -> String {
    let id = id.strip_prefix("usb-").unwrap_or(id);
    let id = match id.rfind("-if") {
        Some(i) if id[i + 3..].chars().next().is_some_and(|c| c.is_ascii_digit()) => &id[..i],
        _ => id,
    };
    id.replace('_', " ")
}

/// The serial ports: those with a stable name (/dev/serial/by-id, named for the device),
/// then the other USB ones. (path, what it is). Listed once a second at most: pages
/// draw every frame.
pub fn ports() -> Vec<(PathBuf, String)> {
    type Cache = Option<(std::time::Instant, Vec<(PathBuf, String)>)>;
    static CACHE: std::sync::Mutex<Cache> = std::sync::Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, v)) = c.as_ref().filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(1)) {
        return v.clone();
    }
    let v = list_ports();
    *c = Some((std::time::Instant::now(), v.clone()));
    v
}

fn list_ports() -> Vec<(PathBuf, String)> {
    let mut named: Vec<(PathBuf, String)> = Vec::new();
    let mut seen = Vec::new();
    for e in fs::read_dir("/dev/serial/by-id").into_iter().flatten().flatten() {
        let target = fs::canonicalize(e.path()).unwrap_or_default();
        let dev = target.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        named.push((e.path(), format!("{} ({dev})", nice(&e.file_name().to_string_lossy()))));
        seen.push(target);
    }
    named.sort_by(|a, b| a.1.cmp(&b.1));
    let mut rest: Vec<(PathBuf, String)> = fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| ["ttyACM", "ttyUSB"].iter().any(|p| e.file_name().to_string_lossy().starts_with(p)))
        .filter(|e| !seen.contains(&e.path()))
        .map(|e| (e.path(), e.file_name().to_string_lossy().into_owned()))
        .collect();
    rest.sort_by(|a, b| a.1.cmp(&b.1));
    named.extend(rest);
    named
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::FromRawFd;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn by_id_names_read_as_the_device() {
        assert_eq!(nice("usb-STMicroelectronics_STM32_STLink_0123ABCD-if02"), "STMicroelectronics STM32 STLink 0123ABCD");
        assert_eq!(nice("usb-FTDI_FT232R_USB_UART_A0000001-if00-port0"), "FTDI FT232R USB UART A0000001");
        assert_eq!(nice("pci-something-iface"), "pci-something-iface");
    }

    #[test]
    fn a_port_reads_and_writes_and_stops_when_dropped() {
        // A pseudo-terminal stands in for the board: its slave is the port.
        let (mut m, mut s) = (0, 0);
        // SAFETY: valid out pointers; name, termios and winsize may be null.
        assert_eq!(unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
        // SAFETY: openpty opened both; each File owns one.
        let (mut board, slave) = unsafe { (File::from_raw_fd(m), File::from_raw_fd(s)) };
        let path = fs::read_link(format!("/proc/self/fd/{s}")).unwrap();
        assert!(Port::open(&path, 12345, Box::new(|_| {})).is_err(), "not a rate");
        let (tx, rx) = channel();
        let mut port = Port::open(&path, 115200, Box::new(move |e| {
            let _ = tx.send(match e {
                Event::Data(d) => d,
                Event::End(why) => format!("<end {why:?}>"),
            });
        }))
        .unwrap();
        board.write_all(b"boot ok\r\n").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "boot ok\r\n", "raw: no translation");
        port.send("led on\n").unwrap();
        let mut got = [0u8; 7];
        board.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"led on\n");
        drop(port);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "<end None>", "closed when asked");
        drop(slave);
    }
}
