//! RS232 link to an EMCOTRONIC TM02: program preparation, sending and receiving.
//!
//! Port of `tools/tm02send.py`. Wire format as found in WINCAM.EXE and EMCO SERTRANS:
//! first line `%NNNN ` (with a trailing space), CRLF line ends, end at `M30`; the control
//! ignores everything before `%`/`O` and pads its own output with NULs or spaces.
//! Port setup as SERTRANS does it: DTR held low, RTS high, CTS gates sending.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

pub const CTRL_Z: u8 = 0x1a;
const CHUNK: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Handshake {
    RtsCts,
    XonXoff,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParityKind {
    Even,
    Odd,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortConfig {
    pub path: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: ParityKind,
    pub stop_bits: u8,
    pub handshake: Handshake,
    /// How long sending may wait for CTS (or XON) before giving up.
    pub stall_secs: u64,
}

impl Default for PortConfig {
    fn default() -> Self {
        // D00=2400, O91=121: ISO, 7 data bits, even parity, 1 stop bit.
        Self {
            path: "/dev/ttyUSB0".into(),
            baud: 2400,
            data_bits: 7,
            parity: ParityKind::Even,
            stop_bits: 1,
            handshake: Handshake::RtsCts,
            stall_secs: 30,
        }
    }
}

impl PortConfig {
    /// Seconds per character on the wire: start bit + data + parity + stop.
    pub fn char_time(&self) -> f64 {
        let bits = 1 + self.data_bits as u32 + u32::from(self.parity != ParityKind::None) + self.stop_bits as u32;
        bits as f64 / self.baud.max(1) as f64
    }

    pub fn summary(&self) -> String {
        let p = match self.parity {
            ParityKind::Even => "E",
            ParityKind::Odd => "O",
            ParityKind::None => "N",
        };
        format!("{} {}{}{}", self.baud, self.data_bits, p, self.stop_bits)
    }
}

#[derive(Debug)]
pub enum LinkError {
    Open(String, serialport::Error),
    Io(io::Error),
    Stalled { sent: usize, secs: u64 },
    Cancelled { done: usize },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Open(path, e) => {
                if e.kind() == serialport::ErrorKind::Io(io::ErrorKind::PermissionDenied) {
                    write!(f, "No permission to open {path}. Add your user to the “dialout” group and log in again.")
                } else if e.kind() == serialport::ErrorKind::NoDevice
                    || e.kind() == serialport::ErrorKind::Io(io::ErrorKind::NotFound)
                {
                    write!(f, "{path} is not connected.")
                } else {
                    write!(f, "Could not open {path}: {e}")
                }
            }
            LinkError::Io(e) => write!(f, "Serial port error: {e}"),
            LinkError::Stalled { sent, secs } => write!(
                f,
                "The control did not accept data for {secs} s after {sent} characters. \
                 Check that it is waiting (O number INP), the cable, and the handshake setting."
            ),
            LinkError::Cancelled { .. } => write!(f, "Cancelled"),
        }
    }
}

impl From<io::Error> for LinkError {
    fn from(e: io::Error) -> Self {
        LinkError::Io(e)
    }
}

/// A program ready to send: the exact bytes, plus what the confirmation shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub bytes: Vec<u8>,
    pub number: u16,
    pub blocks: usize,
    pub has_m30: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrepareError {
    NoNumber,
    NonAscii { line: usize, text: String },
    Empty,
}

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrepareError::NoNumber => write!(f, "The program has no %NNNN start line. Set a program number."),
            PrepareError::NonAscii { line, text } => {
                write!(f, "Line {line} has a character the control cannot read: “{text}”")
            }
            PrepareError::Empty => write!(f, "The file has no program blocks."),
        }
    }
}

/// `%NNNN` or `ONNNN` at the start of a line: (number, digits end).
fn program_start(line: &str) -> Option<(u16, usize)> {
    let t = line.trim_start();
    let lead = line.len() - t.len();
    let rest = t.strip_prefix('%').or_else(|| t.strip_prefix('O'))?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let n = rest[..digits].parse::<u32>().ok()?;
    Some(((n % 10000) as u16, lead + 1 + digits))
}

fn has_m30(line: &str) -> bool {
    let b = line.as_bytes();
    b.windows(3).enumerate().any(|(i, w)| {
        w == b"M30"
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && b.get(i + 3).is_none_or(|c| !c.is_ascii_alphanumeric())
    })
}

/// Text decoded from a file as WinCAM writes it (CP1252); padding from the control removed.
pub fn decode(raw: &[u8]) -> String {
    let end = raw.iter().rposition(|&c| !matches!(c, 0 | b' ' | b'\r' | b'\n' | CTRL_Z)).map_or(0, |i| i + 1);
    raw[..end].iter().map(|&c| c as char).collect()
}

/// Program number found in a file, if any.
pub fn detect_number(text: &str) -> Option<u16> {
    text.lines().find_map(program_start).map(|(n, _)| n)
}

/// Build the bytes to send, as `tm02send.py send` does.
pub fn prepare(text: &str, number: Option<u16>, ctrl_z: bool) -> Result<Prepared, PrepareError> {
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    let n;
    match lines.iter().position(|l| program_start(l).is_some()) {
        Some(start) => {
            lines.drain(..start);
            let (found, end) = program_start(&lines[0]).unwrap();
            n = number.unwrap_or(found);
            let rest = lines[0][end..].trim().to_string();
            // "%NNNN " with a trailing space, as WinCAM and the control write it
            lines[0] = if rest.is_empty() { format!("%{n:04} ") } else { format!("%{n:04} {rest}") };
        }
        None => {
            n = number.ok_or(PrepareError::NoNumber)?;
            lines.retain(|l| !l.trim().is_empty());
            lines.insert(0, format!("%{n:04} "));
        }
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    if lines.len() < 2 {
        return Err(PrepareError::Empty);
    }
    for (i, l) in lines.iter().enumerate() {
        if l.chars().any(|c| !c.is_ascii() || (c.is_ascii_control() && c != '\t')) {
            return Err(PrepareError::NonAscii { line: i + 1, text: l.clone() });
        }
    }
    let m30 = lines.iter().any(|l| has_m30(l));
    let mut bytes = lines.join("\r\n").into_bytes();
    bytes.extend_from_slice(b"\r\n");
    if ctrl_z {
        bytes.push(CTRL_Z);
    }
    Ok(Prepared { bytes, number: n, blocks: lines.len() - 1, has_m30: m30 })
}

/// Index just past the first line containing M30, if complete.
pub fn first_program_end(data: &[u8]) -> Option<usize> {
    let mut start = 0;
    for (i, &c) in data.iter().enumerate() {
        if c == b'\n' {
            if has_m30(&String::from_utf8_lossy(&data[start..i])) {
                return Some(i + 1);
            }
            start = i + 1;
        }
    }
    None
}

/// Received bytes without NUL padding and anything from Ctrl-Z on.
pub fn clean_received(raw: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = raw.iter().copied().filter(|&c| c != 0).collect();
    if let Some(i) = v.iter().position(|&c| c == CTRL_Z) {
        v.truncate(i);
    }
    while v.last() == Some(&b' ') {
        v.pop();
    }
    v
}

/// Bytes that should never come from the control at the right baud rate and parity.
pub fn unexpected_bytes(data: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> =
        data.iter().copied().filter(|&c| c > 0x7e || (c < 0x20 && !matches!(c, b'\r' | b'\n' | b'\t'))).collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedProgram {
    pub number: u16,
    pub bytes: Vec<u8>,
}

impl ReceivedProgram {
    pub fn blocks(&self) -> usize {
        self.bytes.split(|&c| c == b'\n').filter(|l| l.iter().any(|c| !c.is_ascii_whitespace())).count().saturating_sub(1)
    }
}

/// Split received data at each `%NNNN` line. Text before the first one is dropped.
pub fn split_programs(data: &[u8]) -> Vec<ReceivedProgram> {
    let text = String::from_utf8_lossy(data);
    let mut out: Vec<ReceivedProgram> = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with('%')
            && let Some((n, _)) = program_start(line)
        {
            out.push(ReceivedProgram { number: n, bytes: Vec::new() });
        }
        if let Some(p) = out.last_mut() {
            p.bytes.extend_from_slice(line.as_bytes());
        }
    }
    for p in &mut out {
        while p.bytes.last().is_some_and(|c| matches!(c, b' ' | b'\r' | b'\n')) {
            p.bytes.pop();
        }
        p.bytes.extend_from_slice(b"\r\n");
    }
    out
}

pub fn open(cfg: &PortConfig) -> Result<Box<dyn SerialPort>, LinkError> {
    let mut port = serialport::new(&cfg.path, cfg.baud)
        .data_bits(if cfg.data_bits == 8 { DataBits::Eight } else { DataBits::Seven })
        .parity(match cfg.parity {
            ParityKind::Even => Parity::Even,
            ParityKind::Odd => Parity::Odd,
            ParityKind::None => Parity::None,
        })
        .stop_bits(if cfg.stop_bits == 2 { StopBits::Two } else { StopBits::One })
        .flow_control(match cfg.handshake {
            Handshake::RtsCts => FlowControl::Hardware,
            Handshake::XonXoff => FlowControl::Software,
            Handshake::None => FlowControl::None,
        })
        .timeout(Duration::from_millis(200))
        .open()
        .map_err(|e| LinkError::Open(cfg.path.clone(), e))?;
    // SERTRANS: DTR low, RTS high. Ptys and some adapters have no modem lines.
    let _ = port.write_data_terminal_ready(false);
    let _ = port.write_request_to_send(cfg.handshake == Handshake::RtsCts);
    let _ = port.clear(serialport::ClearBuffer::All);
    Ok(port)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModemLines {
    pub cts: bool,
    pub dsr: bool,
    pub cd: bool,
}

pub fn modem_lines(port: &mut dyn SerialPort) -> Option<ModemLines> {
    Some(ModemLines {
        cts: port.read_clear_to_send().ok()?,
        dsr: port.read_data_set_ready().ok()?,
        cd: port.read_carrier_detect().ok()?,
    })
}

/// Send `data`, calling `progress(sent, total)`. Waits while the control holds CTS/XOFF.
pub fn send(
    port: &mut dyn SerialPort,
    data: &[u8],
    stall: Duration,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<(), LinkError> {
    let mut sent = 0;
    let mut last_progress = Instant::now();
    progress(0, data.len());
    while sent < data.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(LinkError::Cancelled { done: sent });
        }
        let end = (sent + CHUNK).min(data.len());
        match port.write(&data[sent..end]) {
            Ok(n) if n > 0 => {
                sent += n;
                last_progress = Instant::now();
                progress(sent, data.len());
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(e.into()),
        }
        if last_progress.elapsed() >= stall {
            return Err(LinkError::Stalled { sent, secs: stall.as_secs() });
        }
    }
    // The driver may still hold the tail while CTS is low.
    let _ = port.flush();
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveMode {
    /// Stop after the first M30 line.
    One,
    /// Keep going until Ctrl-Z or silence.
    Several,
}

/// Receive until M30 (One), Ctrl-Z, `idle` of silence after data, or cancel. Returns the raw bytes;
/// a cancel keeps what arrived (`Ok`), since partial data is still worth saving.
pub fn receive(
    port: &mut dyn SerialPort,
    mode: ReceiveMode,
    idle: Duration,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize),
) -> Result<Vec<u8>, LinkError> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 256];
    let mut last = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                last = Instant::now();
                progress(raw.len());
                if buf[..n].contains(&CTRL_Z) {
                    break;
                }
                if mode == ReceiveMode::One && first_program_end(&clean_received(&raw)).is_some() {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(e.into()),
        }
        if !raw.iter().all(|&c| c == 0) && last.elapsed() >= idle {
            break;
        }
    }
    let _ = port.write_request_to_send(false);
    Ok(raw)
}

/// Turn raw received bytes into programs. `One` keeps only up to the first M30.
pub fn finish_receive(raw: &[u8], mode: ReceiveMode) -> Vec<ReceivedProgram> {
    let mut data = clean_received(raw);
    if mode == ReceiveMode::One
        && let Some(end) = first_program_end(&data)
    {
        data.truncate(end);
    }
    split_programs(&data)
}

/// Loopback test (pins 2 and 3 bridged): returns how many of the test bytes came back intact.
pub fn loopback(port: &mut dyn SerialPort) -> Result<(usize, usize), LinkError> {
    let probe = b"%0000 TM02 LINK LOOPBACK 0123456789 G01 X-1.5 Z2\r\n";
    port.write_all(probe)?;
    let _ = port.flush();
    let mut got = Vec::new();
    let mut buf = [0u8; 64];
    let start = Instant::now();
    while got.len() < probe.len() && start.elapsed() < Duration::from_secs(3) {
        match port.read(&mut buf) {
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
    let ok = probe.iter().zip(&got).take_while(|(a, b)| a == b).count();
    Ok((ok, probe.len()))
}

/// Serial ports worth offering: USB adapters first, then the first four built-in ports.
pub fn list_ports() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut by_id = std::collections::HashMap::new();
    if let Ok(rd) = std::fs::read_dir("/dev/serial/by-id") {
        for e in rd.flatten() {
            if let Ok(target) = std::fs::canonicalize(e.path()) {
                let name = e.file_name().to_string_lossy().trim_start_matches("usb-").to_string();
                let name = name.split("-if").next().unwrap_or(&name).replace('_', " ");
                by_id.insert(target.to_string_lossy().to_string(), name);
            }
        }
    }
    let mut paths: Vec<String> = std::fs::read_dir("/dev")
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("ttyUSB") || n.starts_with("ttyACM"))
                .map(|n| format!("/dev/{n}"))
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    for p in paths {
        let label = by_id.get(&p).cloned().unwrap_or_else(|| "USB serial adapter".into());
        out.push((p, label));
    }
    for i in 0..4 {
        let p = format!("/dev/ttyS{i}");
        if std::path::Path::new(&p).exists() {
            out.push((p, "Built-in serial port".into()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const WINCAM: &str = "(* WinCAM by EMCO [c] 1995-2001,  NC-Programm EMCOTRONIC TM02 T*)\r\n \r\n\
        N0005 G53 T0000\r\nN0010 G54\r\nN0035 M30\r\n";

    #[test]
    fn wincam_export_gets_header() {
        let p = prepare(WINCAM, Some(1234), false).unwrap();
        let s = String::from_utf8(p.bytes).unwrap();
        assert!(s.starts_with("%1234 \r\n(* WinCAM"));
        assert!(s.ends_with("N0035 M30\r\n"));
        assert_eq!(p.blocks, 4);
        assert!(p.has_m30);
    }

    #[test]
    fn no_number_is_an_error() {
        assert_eq!(prepare(WINCAM, None, false), Err(PrepareError::NoNumber));
    }

    #[test]
    fn received_file_keeps_its_number_and_loses_padding() {
        let raw = b"%0060 \r\nN0000  G71 G95 G54 M39\r\nN0190  G53 T0000 M30\r\n\0\0\0   ";
        let text = decode(raw);
        assert_eq!(detect_number(&text), Some(60));
        let p = prepare(&text, None, true).unwrap();
        assert_eq!(p.bytes, b"%0060 \r\nN0000  G71 G95 G54 M39\r\nN0190  G53 T0000 M30\r\n\x1a");
        let p = prepare(&text, Some(61), false).unwrap();
        assert!(p.bytes.starts_with(b"%0061 \r\n"));
    }

    #[test]
    fn junk_before_start_is_dropped() {
        let p = prepare("garbage\nO77\nN10 G00 X1\nM30\n", None, false).unwrap();
        assert!(p.bytes.starts_with(b"%0077 \r\nN10"));
    }

    #[test]
    fn m30_word_boundaries() {
        assert!(has_m30("N0160  G53 T0000 M30"));
        assert!(!has_m30("N0160 M300"));
        assert!(!has_m30("N0160 XM30"));
    }

    #[test]
    fn non_ascii_rejected() {
        let e = prepare("%0001\nN10 (Länge)\nM30\n", None, false).unwrap_err();
        assert!(matches!(e, PrepareError::NonAscii { line: 2, .. }));
    }

    #[test]
    fn split_and_clean() {
        let raw = b"\0\0%0060 \r\nN0000  G71\r\nN0190  G53 T0000 M30\r\n\0\0%0061 \r\nN0000  G71\r\nM30\r\n   \x1azz";
        let all = finish_receive(raw, ReceiveMode::Several);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].number, 60);
        assert_eq!(all[1].bytes, b"%0061 \r\nN0000  G71\r\nM30\r\n");
        assert_eq!(all[0].blocks(), 2);
        let one = finish_receive(raw, ReceiveMode::One);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].number, 60);
        assert!(unexpected_bytes(&clean_received(raw)).is_empty());
        assert_eq!(unexpected_bytes(&[0xe5, b'A', 0x8d, 0xe5]), vec![0x8d, 0xe5]);
    }

    #[test]
    fn char_time() {
        let c = PortConfig::default();
        assert!((c.char_time() - 10.0 / 2400.0).abs() < 1e-9);
        assert_eq!(c.summary(), "2400 7E1");
    }

    #[cfg(unix)]
    #[test]
    fn send_and_receive_over_pty() {
        use serialport::TTYPort;
        let (mut a, mut b) = TTYPort::pair().unwrap();
        a.set_timeout(Duration::from_millis(100)).unwrap();
        b.set_timeout(Duration::from_millis(100)).unwrap();
        let p = prepare("%0060\nN10 G00 X1\nN20 M30\n", None, false).unwrap();
        let data = p.bytes.clone();
        let t = std::thread::spawn(move || {
            let cancel = AtomicBool::new(false);
            receive(&mut b, ReceiveMode::One, Duration::from_secs(2), &cancel, |_| {}).unwrap()
        });
        std::thread::sleep(Duration::from_millis(200));
        let cancel = AtomicBool::new(false);
        let mut seen = 0;
        send(&mut a, &data, Duration::from_secs(5), &cancel, |s, _| seen = s).unwrap();
        assert_eq!(seen, data.len());
        let got = finish_receive(&t.join().unwrap(), ReceiveMode::One);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].bytes, data);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_keeps_partial_receive() {
        use serialport::TTYPort;
        let (mut a, mut b) = TTYPort::pair().unwrap();
        b.set_timeout(Duration::from_millis(50)).unwrap();
        a.write_all(b"%0001 \r\nN10").unwrap();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let t = std::thread::spawn(move || receive(&mut b, ReceiveMode::Several, Duration::from_secs(30), &c2, |_| {}));
        std::thread::sleep(Duration::from_millis(300));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(t.join().unwrap().unwrap(), b"%0001 \r\nN10");
    }
}
