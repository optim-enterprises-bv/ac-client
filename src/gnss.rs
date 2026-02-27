//! GNSS/GPS receiver via serial port.
//!
//! Reads NMEA 0183 sentences from a serial device (e.g. `/dev/ttyUSB0`),
//! parses GPRMC and GPGGA sentences, and stores the latest position in a
//! shared `Arc<Mutex<Option<GnssPosition>>>`.
//!
//! The reader runs in a `spawn_blocking` task so it never blocks the async
//! runtime.  If the device is absent the reader exits silently and position
//! stays `None`.

use std::io::{self, BufRead, BufReader};
use std::fs;
use std::sync::{Arc, Mutex};

use log::{debug, warn};
use nix::sys::termios::{
    self, BaudRate, ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg,
};

/// Latest GNSS position fix.
#[derive(Debug, Clone)]
pub struct GnssPosition {
    pub latitude:  String,
    pub longitude: String,
}

/// Spawns a background serial reader.  Position is updated in-place.
/// Returns a handle to the shared position state.
pub fn spawn_gnss_reader(device: &str, baud: u32) -> Arc<Mutex<Option<GnssPosition>>> {
    let position: Arc<Mutex<Option<GnssPosition>>> = Arc::new(Mutex::new(None));
    let pos_clone = Arc::clone(&position);
    let device = device.to_string();

    tokio::task::spawn_blocking(move || {
        if let Err(e) = gnss_reader_loop(&device, baud, pos_clone) {
            warn!("GNSS reader on {device} exited: {e}");
        }
    });

    position
}

fn gnss_reader_loop(
    device:   &str,
    baud:     u32,
    position: Arc<Mutex<Option<GnssPosition>>>,
) -> io::Result<()> {
    let file = fs::OpenOptions::new().read(true).open(device)?;
    configure_serial(&file, baud)?;

    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => { warn!("GNSS read error: {e}"); break; }
        };
        if let Some(pos) = parse_nmea(&line) {
            debug!("GNSS fix: lat={} lon={}", pos.latitude, pos.longitude);
            if let Ok(mut guard) = position.lock() {
                *guard = Some(pos);
            }
        }
    }
    Ok(())
}

/// Configure the serial port for raw NMEA reading (8N1, no echo, no signals).
fn configure_serial(file: &fs::File, baud: u32) -> io::Result<()> {
    // `&fs::File` implements `AsFd`, which is what nix 0.29 termios functions require.
    let mut t = termios::tcgetattr(file)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    // Raw input: no canonical mode, no echo, no signals
    t.local_flags &= !(
        LocalFlags::ICANON |
        LocalFlags::ECHO   |
        LocalFlags::ECHOE  |
        LocalFlags::ISIG
    );
    // No output processing
    t.output_flags &= !OutputFlags::OPOST;
    // Disable software flow control and strip/parity
    t.input_flags &= !(
        InputFlags::IXON   |
        InputFlags::IXOFF  |
        InputFlags::IXANY  |
        InputFlags::ISTRIP |
        InputFlags::INPCK
    );
    // 8 data bits, no parity, 1 stop bit, enable receiver, ignore modem ctrl
    t.control_flags |= ControlFlags::CS8 | ControlFlags::CREAD | ControlFlags::CLOCAL;
    t.control_flags &= !(ControlFlags::CSIZE | ControlFlags::CSTOPB | ControlFlags::PARENB);

    // VMIN=1, VTIME=0: blocking read of at least 1 byte
    t.control_chars[nix::sys::termios::SpecialCharacterIndices::VMIN as usize] = 1;
    t.control_chars[nix::sys::termios::SpecialCharacterIndices::VTIME as usize] = 0;

    let baud_rate = match baud {
        1200   => BaudRate::B1200,
        2400   => BaudRate::B2400,
        4800   => BaudRate::B4800,
        9600   => BaudRate::B9600,
        19200  => BaudRate::B19200,
        38400  => BaudRate::B38400,
        57600  => BaudRate::B57600,
        115200 => BaudRate::B115200,
        _      => BaudRate::B9600,
    };

    termios::cfsetospeed(&mut t, baud_rate)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    termios::cfsetispeed(&mut t, baud_rate)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    termios::tcsetattr(file, SetArg::TCSANOW, &t)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    Ok(())
}

// ── NMEA sentence parser ──────────────────────────────────────────────────────

/// Attempt to extract a position fix from a single NMEA sentence.
/// Handles GPRMC and GPGGA (and their GNRMC/GNGGA multi-constellation variants).
fn parse_nmea(line: &str) -> Option<GnssPosition> {
    let line = line.trim();
    // Validate checksum if present
    if line.contains('*') {
        if !nmea_checksum_ok(line) {
            return None;
        }
    }
    // Strip leading '$' and trailing checksum
    let sentence = line.trim_start_matches('$');
    let sentence = sentence.split('*').next().unwrap_or(sentence);
    let fields: Vec<&str> = sentence.split(',').collect();
    if fields.is_empty() {
        return None;
    }

    match fields[0] {
        // GPRMC / GNRMC — Recommended Minimum Specific GNSS Data
        "GPRMC" | "GNRMC" => parse_rmc(&fields),
        // GPGGA / GNGGA — Global Positioning System Fix Data
        "GPGGA" | "GNGGA" => parse_gga(&fields),
        _ => None,
    }
}

/// Parse a GPRMC sentence: $GPRMC,HHMMSS.ss,A,LLLL.ll,a,YYYYY.yy,a,...
fn parse_rmc(f: &[&str]) -> Option<GnssPosition> {
    if f.len() < 7 {
        return None;
    }
    // field[2] == "A" means valid fix
    if f[2] != "A" {
        return None;
    }
    let lat = nmea_to_decimal(f[3], f[4])?;
    let lon = nmea_to_decimal(f[5], f[6])?;
    Some(GnssPosition {
        latitude:  format!("{lat:.6}"),
        longitude: format!("{lon:.6}"),
    })
}

/// Parse a GPGGA sentence: $GPGGA,HHMMSS.ss,LLLL.ll,a,YYYYY.yy,a,q,...
fn parse_gga(f: &[&str]) -> Option<GnssPosition> {
    if f.len() < 7 {
        return None;
    }
    // field[6] is fix quality: 0 = invalid
    if f[6] == "0" || f[6].is_empty() {
        return None;
    }
    let lat = nmea_to_decimal(f[2], f[3])?;
    let lon = nmea_to_decimal(f[4], f[5])?;
    Some(GnssPosition {
        latitude:  format!("{lat:.6}"),
        longitude: format!("{lon:.6}"),
    })
}

/// Convert NMEA coordinate (DDDMM.mmm) + hemisphere indicator to decimal degrees.
fn nmea_to_decimal(coord: &str, hemi: &str) -> Option<f64> {
    if coord.is_empty() {
        return None;
    }
    // Find the decimal point to split degrees from minutes
    let dot = coord.find('.')?;
    if dot < 2 {
        return None;
    }
    let deg_digits = dot - 2;
    let degrees: f64 = coord[..deg_digits].parse().ok()?;
    let minutes: f64 = coord[deg_digits..].parse().ok()?;
    let mut decimal = degrees + minutes / 60.0;
    if hemi == "S" || hemi == "W" {
        decimal = -decimal;
    }
    Some(decimal)
}

/// Validate the XOR checksum of an NMEA sentence (the part between $ and *).
fn nmea_checksum_ok(sentence: &str) -> bool {
    let inner = sentence.trim_start_matches('$');
    let mut parts = inner.splitn(2, '*');
    let body = match parts.next() {
        Some(b) => b,
        None => return false,
    };
    let expected_hex = match parts.next() {
        Some(h) => h.trim(),
        None => return false,
    };
    let expected: u8 = match u8::from_str_radix(expected_hex, 16) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let actual: u8 = body.bytes().fold(0u8, |acc, b| acc ^ b);
    actual == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gprmc() {
        let line = "$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";
        let pos = parse_nmea(line).unwrap();
        assert!(pos.latitude.starts_with("48."), "lat={}", pos.latitude);
        assert!(pos.longitude.starts_with("11."), "lon={}", pos.longitude);
    }

    #[test]
    fn parse_gpgga() {
        let line = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
        let pos = parse_nmea(line).unwrap();
        assert!(pos.latitude.starts_with("48."), "lat={}", pos.latitude);
    }

    #[test]
    fn invalid_fix_ignored() {
        // V = invalid fix
        let line = "$GPRMC,123519,V,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A";
        assert!(parse_nmea(line).is_none());
    }
}
