//! Minimal SNTP client: ask one server for the time and set the RTC.
//!
//! The RTC is what MbedTLS reads to check certificate dates. This is a spike
//! helper; the real firmware will keep the clock in sync on a schedule.

use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Stack};
use embassy_time::{Duration, Instant, with_timeout};
use esp_hal::rtc_cntl::Rtc;

/// Seconds between 1900-01-01 (NTP) and 1970-01-01 (Unix).
const NTP_TO_UNIX: u64 = 2_208_988_800;
/// Anything before 2025-01-01 is certainly not the real time.
const PLAUSIBLE_AFTER: u64 = 1_735_689_600;

/// Try the server up to six times; true when the RTC now holds a plausible time.
pub async fn sync(stack: Stack<'static>, host: &str, rtc: &Rtc<'_>) -> bool {
    let addrs = match stack.dns_query(host, DnsQueryType::A).await {
        Ok(addrs) if !addrs.is_empty() => addrs,
        other => {
            log::error!("sntp: cannot resolve {host}: {other:?}");
            log::error!(
                "TIME NOT SET: certificate dates cannot be checked, expect the TLS handshake to fail"
            );
            return false;
        }
    };

    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 128];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    if let Err(e) = socket.bind(0) {
        log::error!("sntp: cannot open a UDP socket: {e:?}");
        return false;
    }

    for attempt in 0..6 {
        let server = addrs[attempt % addrs.len()];
        // LI 0, version 4, mode 3 (client); the rest of the 48 bytes stays zero.
        let mut request = [0u8; 48];
        request[0] = 0x23;
        let sent = Instant::now();
        if socket
            .send_to(&request, IpEndpoint::new(server, 123))
            .await
            .is_err()
        {
            continue;
        }
        let mut reply = [0u8; 48];
        let received = with_timeout(Duration::from_secs(3), socket.recv_from(&mut reply)).await;
        let Ok(Ok((len, _))) = received else {
            log::warn!("sntp: no reply from {server} (attempt {})", attempt + 1);
            continue;
        };
        // Mode must be 4 (server) and the stratum non-zero (zero is a "kiss of death").
        if len < 48 || reply[0] & 7 != 4 || reply[1] == 0 {
            log::warn!("sntp: unusable reply from {server}");
            continue;
        }
        let secs = u64::from(u32::from_be_bytes([
            reply[40], reply[41], reply[42], reply[43],
        ]));
        let frac = u64::from(u32::from_be_bytes([
            reply[44], reply[45], reply[46], reply[47],
        ]));
        let Some(unix) = secs.checked_sub(NTP_TO_UNIX) else {
            continue;
        };
        let micros = (frac * 1_000_000) >> 32;
        // Half the round trip is the best guess for the network delay.
        let delay_us = sent.elapsed().as_micros() / 2;
        rtc.set_current_time_us(unix * 1_000_000 + micros + delay_us);

        log::info!(
            "sntp: {} (round trip {} ms, stratum {})",
            format_utc(unix),
            sent.elapsed().as_millis(),
            reply[1]
        );
        if unix < PLAUSIBLE_AFTER {
            log::error!(
                "TIME WRONG: the server says {}, which is in the past. Certificates will look expired or not yet valid.",
                format_utc(unix)
            );
            return false;
        }
        return true;
    }
    log::error!(
        "TIME NOT SET: no SNTP answer from {host}. Certificate dates cannot be checked, expect the TLS handshake to fail."
    );
    false
}

/// `2026-10-09 08:13:51 UTC` from Unix seconds, without a calendar library.
pub fn format_utc(unix: u64) -> heapless::String<32> {
    use core::fmt::Write;
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm), valid for all dates after 1970.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let mut out = heapless::String::new();
    let _ = write!(
        out,
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    );
    out
}
