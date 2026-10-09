//! TLS spike: can this ESP32-S3 do an HTTPS request with full certificate
//! verification, and what does it cost?
//!
//! Needs no screen, button or MQTT broker. It joins Wi-Fi with the same
//! `config.toml` as the panel firmware, sets the clock over SNTP (certificates
//! have validity dates), then does `[tls_spike] runs` HTTPS GET requests and
//! prints timings and heap numbers over the serial port, plus one summary block
//! to paste back. See README.md, "TLS spike".
//!
//! NOT TESTED ON HARDWARE. It builds and lints in CI; nothing here has run on a board yet.

#![no_std]
#![no_main]
// The shared config module also carries the panel's settings, which this binary does not read.
#![allow(dead_code)]

extern crate alloc;

#[path = "../config.rs"]
mod config;
#[path = "../net.rs"]
mod net;

mod http;
mod sntp;

use alloc::ffi::CString;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::rtc_cntl::Rtc;
use esp_hal::timer::timg::TimerGroup;
use heapless::Vec;
use mbedtls_rs::sys::hook::backend::embassy::timer::EmbassyTimer;
use mbedtls_rs::sys::hook::backend::esp::EspAccel;
use mbedtls_rs::sys::hook::backend::esp::wall_clock::EspRtcWallClock;
use mbedtls_rs::{Certificate, ClientSessionConfig, Session, SessionConfig, Tls, X509};
use static_cell::StaticCell;
use tinyrlibc as _; // C library functions the precompiled MbedTLS links against

use config::{CompiledConfig, ConfigSource};
use http::{Reader, Url};

esp_bootloader_esp_idf::esp_app_desc!();

/// The trusted root certificates (see certs/README.md), as a C string for MbedTLS.
const ROOTS_PEM: &core::ffi::CStr = match core::ffi::CStr::from_bytes_with_nul(
    concat!(include_str!("../../certs/roots.pem"), "\0").as_bytes(),
) {
    Ok(roots) => roots,
    Err(_) => panic!("certs/roots.pem is not valid text"),
};

/// Heap: 64 KB of RAM the bootloader is done with, plus 160 KB of normal RAM.
/// The panel firmware uses 64 + 96 KB, so this shows what TLS needs on top.
const HEAP_RECLAIMED: usize = 64 * 1024;
const HEAP_MAIN: usize = 160 * 1024;

/// Seconds between requests, so the radio and the server settle.
const PAUSE_BETWEEN_RUNS_S: u64 = 3;
/// Give up on one whole request (DNS to close) after this long.
const RUN_TIMEOUT_S: u64 = 45;

static WALL_CLOCK: StaticCell<EspRtcWallClock<&'static Rtc<'static>>> = StaticCell::new();
static TIMER: StaticCell<EmbassyTimer> = StaticCell::new();
static RTC: StaticCell<Rtc<'static>> = StaticCell::new();
static TRNG: StaticCell<Trng> = StaticCell::new();

/// What one request measured. Times are milliseconds, heap values bytes.
#[derive(Clone, Copy, Default)]
struct RunResult {
    ok: bool,
    status: u16,
    dns_ms: u64,
    tcp_ms: u64,
    handshake_ms: u64,
    first_byte_ms: u64,
    total_ms: u64,
    bytes: usize,
    heap_free_before: usize,
    heap_free_in_session: usize,
    heap_free_after: usize,
    /// Highest heap use seen since boot, read right after the run.
    heap_peak_used: usize,
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    log::info!("DeskWatch TLS spike {}", env!("CARGO_PKG_VERSION"));

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: HEAP_RECLAIMED);
    esp_alloc::heap_allocator!(size: HEAP_MAIN);
    log::info!("heap: {} KB total", (HEAP_RECLAIMED + HEAP_MAIN) / 1024);

    // The scheduler must run before the radio starts.
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    let cfg =
        match CompiledConfig.load() {
            Ok(cfg) => cfg,
            Err(e) => halt(format_args!(
                "config: {e:?}. Copy config.example.toml to config.toml, fill it in and rebuild."
            ))
            .await,
        };
    let spike = match CompiledConfig.load_spike() {
        Ok(spike) => spike,
        Err(e) => halt(format_args!("config [tls_spike]: {e:?}")).await,
    };
    let url = match Url::parse(&spike.url) {
        Ok(url) => url,
        Err(e) => halt(format_args!("tls_spike.url: {e}")).await,
    };
    let Ok(server_name) = CString::new(url.host) else {
        halt(format_args!("tls_spike.url: bad host name")).await
    };
    log::info!(
        "target: https://{}:{}{} | {} run(s) | hardware crypto: {}",
        url.host,
        url.port,
        url.path,
        spike.runs,
        spike.hw_accel
    );

    // MbedTLS needs a monotonic timer and a wall clock (for the certificate
    // dates). The RTC is the wall clock: SNTP sets it below. Until then it
    // reports "no time" and certificate checks fail closed.
    let timer = TIMER.init(EmbassyTimer);
    unsafe { mbedtls_rs::sys::hook::timer::hook_timer(Some(timer)) };
    let rtc: &'static Rtc<'static> = RTC.init(Rtc::new(peripherals.LPWR));
    let clock = WALL_CLOCK.init(EspRtcWallClock::new(rtc));
    unsafe { mbedtls_rs::sys::hook::wall_clock::hook_wall_clock(Some(clock)) };

    // The hardware crypto units, if enabled. Hooking an algorithm whose unit is
    // not being serviced would hang the first TLS call, so servicing and hooking
    // are done together here, once, before any TLS state exists.
    let mut accel = EspAccel::new();
    if spike.hw_accel {
        accel = accel
            .with_sha(peripherals.SHA)
            .with_rsa(peripherals.RSA)
            .with_aes(peripherals.AES);
    }
    let accel_queue = accel.start();
    let _hooked = unsafe { accel_queue.hook() };

    // True random numbers need the TRNG source alive for as long as they are used.
    let _trng_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let trng = TRNG.init(Trng::try_new().expect("trng"));
    let mut tls = Tls::new(trng).expect("tls init");

    let stack = net::start(&spawner, peripherals.WIFI, &cfg);
    wait_for_network(stack).await;

    // Time first: without it every certificate looks expired or not yet valid.
    let clock_ok = sntp::sync(stack, &spike.ntp_host, rtc).await;

    // Parse the root certificates once and report what that costs in RAM.
    let heap_before_roots = used();
    let roots = match Certificate::new(X509::PEM(ROOTS_PEM)) {
        Ok(roots) => roots,
        Err(e) => halt(format_args!("root certificates: {e:?}")).await,
    };
    let root_count = ROOTS_PEM
        .to_bytes()
        .windows(27)
        .filter(|w| *w == b"-----BEGIN CERTIFICATE-----")
        .count();
    let roots_ram = used().saturating_sub(heap_before_roots);
    log::info!(
        "roots: {root_count} certificates in the bundle ({} bytes of PEM in flash), {roots_ram} bytes of RAM once parsed",
        ROOTS_PEM.to_bytes().len()
    );

    let mut results: Vec<RunResult, 10> = Vec::new();
    for run in 1..=spike.runs {
        log::info!("---- run {run} of {} ----", spike.runs);
        let session_config = SessionConfig::Client(ClientSessionConfig {
            ca_chain: Some(roots.clone()),
            server_name: Some(server_name.as_c_str()),
            // Full verification: chain, host name and validity dates must all pass.
            ..ClientSessionConfig::new()
        });
        let result = with_timeout(
            Duration::from_secs(RUN_TIMEOUT_S),
            one_request(stack, &mut tls, &session_config, &url),
        )
        .await
        .unwrap_or_else(|_| {
            log::error!("run {run}: timed out after {RUN_TIMEOUT_S} s");
            RunResult::default()
        });
        let _ = results.push(result);
        if run < spike.runs {
            Timer::after(Duration::from_secs(PAUSE_BETWEEN_RUNS_S)).await;
        }
    }

    summary(&results, clock_ok, roots_ram, root_count, rtc);
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}

/// One complete request: DNS, TCP, TLS handshake, GET, read to the end, close.
async fn one_request(
    stack: Stack<'static>,
    tls: &mut Tls<'static>,
    session_config: &SessionConfig<'_>,
    url: &Url<'_>,
) -> RunResult {
    let mut r = RunResult {
        heap_free_before: free(),
        ..RunResult::default()
    };
    let t0 = Instant::now();
    let ms = |t: Instant| t.elapsed().as_millis();

    // DNS.
    let ip = match stack.dns_query(url.host, DnsQueryType::A).await {
        Ok(addrs) if !addrs.is_empty() => addrs[0],
        other => {
            log::error!("dns: no address for {}: {other:?}", url.host);
            return r;
        }
    };
    r.dns_ms = ms(t0);
    log::info!("dns: ok in {} ms", r.dns_ms);

    // TCP. Small socket buffers; TLS records arrive in pieces.
    let mut rx = [0u8; 4096];
    let mut tx = [0u8; 2048];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(20)));
    let t_tcp = Instant::now();
    if let Err(e) = socket.connect((ip, url.port)).await {
        log::error!("tcp: connect failed: {e:?}");
        return r;
    }
    r.tcp_ms = ms(t_tcp);
    log::info!("tcp: connected in {} ms", r.tcp_ms);

    // TLS handshake, with certificate verification required.
    let mut session = match Session::new(tls.reference(), socket, session_config) {
        Ok(session) => session,
        Err(e) => {
            log::error!("tls: cannot create a session: {e}");
            return r;
        }
    };
    log::info!("handshake: starting (free heap {} bytes)", free());
    let t_hs = Instant::now();
    let handshake = session.connect().await;
    r.handshake_ms = ms(t_hs);
    if let Err(e) = handshake {
        log::error!("handshake: FAILED after {} ms: {e}", r.handshake_ms);
        explain_verify_failure(session.tls_verification_details());
        r.heap_peak_used = peak_used();
        return r;
    }
    r.heap_free_in_session = free();
    log::info!(
        "handshake: ok in {} ms, certificate verified (free heap {} bytes)",
        r.handshake_ms,
        r.heap_free_in_session
    );

    // GET, read until the server closes.
    let t_req = Instant::now();
    let mut reader = Reader::new();
    let outcome = http::get(&mut session, url, &mut reader, &t_req).await;
    r.first_byte_ms = reader.first_byte_ms;
    r.status = reader.status;
    r.bytes = reader.total;
    match outcome {
        Ok(()) => r.ok = reader.status != 0,
        Err(e) => {
            // Many servers just close the socket without a TLS close_notify; if
            // the response arrived complete enough to have a status, that is fine.
            if reader.status != 0 {
                log::warn!("read: ended with {e} after a response was received");
                r.ok = true;
            } else {
                log::error!("read: failed: {e}");
            }
        }
    }
    let _ = session.close().await;
    drop(session);
    r.total_ms = ms(t0);
    r.heap_free_after = free();
    r.heap_peak_used = peak_used();

    log::info!(
        "http: status {}, {} bytes ({} of body) | first byte {} ms, total {} ms",
        r.status,
        r.bytes,
        reader.body_bytes(),
        r.first_byte_ms,
        r.total_ms
    );
    if let Some(line) = reader.head_lines() {
        log::info!("http: response head:\n{line}");
    }
    r
}

/// Turn the MbedTLS verification bit mask into words.
fn explain_verify_failure(flags: u32) {
    use mbedtls_rs::sys::{
        MBEDTLS_X509_BADCERT_BAD_KEY, MBEDTLS_X509_BADCERT_BAD_MD, MBEDTLS_X509_BADCERT_BAD_PK,
        MBEDTLS_X509_BADCERT_CN_MISMATCH, MBEDTLS_X509_BADCERT_EXPIRED,
        MBEDTLS_X509_BADCERT_FUTURE, MBEDTLS_X509_BADCERT_NOT_TRUSTED,
    };
    if flags == 0 {
        log::error!("verify: no certificate problem recorded (see the error above)");
        return;
    }
    log::error!("verify: certificate check failed, flags 0x{flags:x}");
    for (bit, text) in [
        (
            MBEDTLS_X509_BADCERT_NOT_TRUSTED,
            "NOT TRUSTED: the chain does not end in one of the bundled root certificates (a root is missing, or the server uses a private CA)",
        ),
        (
            MBEDTLS_X509_BADCERT_EXPIRED,
            "EXPIRED: a certificate is past its end date. If it is not really expired, the CLOCK IS WRONG (too late, or never set)",
        ),
        (
            MBEDTLS_X509_BADCERT_FUTURE,
            "NOT YET VALID: a certificate starts in the future. The CLOCK IS WRONG (too early, or never set)",
        ),
        (
            MBEDTLS_X509_BADCERT_CN_MISMATCH,
            "HOST NAME MISMATCH: the certificate is for another name than the one requested",
        ),
        (
            MBEDTLS_X509_BADCERT_BAD_MD,
            "weak or unsupported hash algorithm in the chain",
        ),
        (
            MBEDTLS_X509_BADCERT_BAD_PK,
            "weak or unsupported public key algorithm in the chain",
        ),
        (
            MBEDTLS_X509_BADCERT_BAD_KEY,
            "weak or unsupported key size in the chain",
        ),
    ] {
        if flags & bit != 0 {
            log::error!("verify:   - {text}");
        }
    }
}

/// The result block to paste back to the project chat.
fn summary(
    results: &[RunResult],
    clock_ok: bool,
    roots_ram: usize,
    root_count: usize,
    rtc: &Rtc<'_>,
) {
    let heap_total = esp_alloc::HEAP.stats().size;
    log::info!("");
    log::info!("======== DESKWATCH TLS SPIKE RESULT (copy from here) ========");
    log::info!("firmware: {} | board: ESP32-S3", env!("CARGO_PKG_VERSION"));
    log::info!("heap_total_bytes: {heap_total}");
    log::info!(
        "clock_set_by_sntp: {clock_ok} | now_utc: {}",
        sntp::format_utc(rtc.current_time_us() / 1_000_000)
    );
    log::info!("root_certs: {root_count} | root_ram_bytes: {roots_ram}");
    for (i, r) in results.iter().enumerate() {
        let n = i + 1;
        log::info!(
            "run {n}: ok={} status={} dns={}ms tcp={}ms handshake={}ms first_byte={}ms total={}ms bytes={}",
            r.ok,
            r.status,
            r.dns_ms,
            r.tcp_ms,
            r.handshake_ms,
            r.first_byte_ms,
            r.total_ms,
            r.bytes
        );
        log::info!(
            "run {n}: free_heap before={} in_session={} after={} | tls_session_cost={} | peak_used_since_boot={}",
            r.heap_free_before,
            r.heap_free_in_session,
            r.heap_free_after,
            r.heap_free_before.saturating_sub(r.heap_free_in_session),
            r.heap_peak_used
        );
    }
    if let [first, .., last] = results {
        let leaked = first.heap_free_after.saturating_sub(last.heap_free_after);
        log::info!(
            "repeatability: free heap after run 1 = {}, after run {} = {}, drift = {} bytes",
            first.heap_free_after,
            results.len(),
            last.heap_free_after,
            leaked
        );
    }
    let all_ok = !results.is_empty() && results.iter().all(|r| r.ok);
    log::info!(
        "verdict: {}",
        if all_ok {
            "ALL RUNS OK"
        } else {
            "AT LEAST ONE RUN FAILED"
        }
    );
    log::info!("======== END OF RESULT ========");
}

async fn wait_for_network(stack: Stack<'static>) {
    log::info!(
        "waiting for Wi-Fi and an IP address (2.4 GHz network, check ssid and password if this takes long)"
    );
    let mut waited = 0u32;
    loop {
        if with_timeout(Duration::from_secs(10), stack.wait_config_up())
            .await
            .is_ok()
        {
            break;
        }
        waited += 10;
        log::warn!("still no IP address after {waited} s");
    }
    if let Some(cfg) = stack.config_v4() {
        log::info!("network: got address {}", cfg.address);
    }
}

fn used() -> usize {
    esp_alloc::HEAP.used()
}

fn free() -> usize {
    esp_alloc::HEAP.free()
}

/// Highest heap use since boot (esp-alloc's `internal-heap-stats`). Wi-Fi start-up
/// also counts, so compare it with the "free before" number: the TLS part of the
/// peak is only meaningful when this run set a new high.
fn peak_used() -> usize {
    esp_alloc::HEAP.stats().max_usage
}

/// Log a fatal problem forever (there is no screen to show it on).
async fn halt(msg: core::fmt::Arguments<'_>) -> ! {
    loop {
        log::error!("{msg}");
        Timer::after(Duration::from_secs(10)).await;
    }
}
