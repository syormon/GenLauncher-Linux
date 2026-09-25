//! SNTP clock check. S3 request signatures fail when the system clock drifts,
//! so the launcher warns before starting a download. Port of `TimeUtility`.

use std::net::UdpSocket;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const NTP_SERVER: &str = "time.windows.com:123";
/// Seconds between the NTP epoch (1900) and the Unix epoch (1970).
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// Unix time reported by the NTP server, or `None` when it cannot be reached.
pub fn network_time() -> Option<SystemTime> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    socket.set_write_timeout(Some(Duration::from_secs(3))).ok()?;

    let mut packet = [0u8; 48];
    // LI = 0, VN = 3, Mode = 3 (client).
    packet[0] = 0x1B;

    socket.send_to(&packet, NTP_SERVER).ok()?;
    let (n, _) = socket.recv_from(&mut packet).ok()?;
    if n < 48 {
        return None;
    }

    // Transmit timestamp lives at offset 40, big-endian seconds + fraction.
    let seconds = u32::from_be_bytes(packet[40..44].try_into().ok()?) as u64;
    let fraction = u32::from_be_bytes(packet[44..48].try_into().ok()?) as u64;
    if seconds == 0 {
        return None;
    }

    let unix_seconds = seconds.checked_sub(NTP_UNIX_OFFSET)?;
    let nanos = (fraction * 1_000_000_000) >> 32;
    Some(UNIX_EPOCH + Duration::from_secs(unix_seconds) + Duration::from_nanos(nanos))
}

/// True when the system clock is more than 15 minutes away from network time.
/// A server we cannot reach is treated as "in sync", as in the original.
pub fn is_system_time_out_of_sync() -> bool {
    let Some(network) = network_time() else { return false };
    let now = SystemTime::now();

    let drift = match network.duration_since(now) {
        Ok(d) => d,
        Err(e) => e.duration(),
    };

    drift >= Duration::from_secs(15 * 60)
}
