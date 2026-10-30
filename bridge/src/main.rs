//! DeskWatch bridge: collects data on the home server and serves the dashboard
//! page. MQTT carries alerts in (`docs/mqtt-schema.md`).
//!
//! The main loop owns all state, so there are no locks. Sources and the MQTT
//! connection run as tasks and send what they learn over channels:
//!
//! - every second, and after every event, the kiosk page gets a fresh snapshot
//!   of what the composer knows;
//! - each `[[source.*]]` block runs as its own task and sends fact updates
//!   and its health as `SourceMsg`s;
//! - alerts arrive from the MQTT task and `POST /api/alert` as `Inbound`s.
//!
//! With `--demo` no sources run: a scripted loop of fake data feeds the
//! composer instead, for watching the dashboard page.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use deskwatch_bridge::alerts::Alerts;
use deskwatch_bridge::composer::Composer;
use deskwatch_bridge::config::Config;
use deskwatch_bridge::demo::Demo;
use deskwatch_bridge::health;
use deskwatch_bridge::hooks::Hooks;
use deskwatch_bridge::kiosk::Kiosk;
use deskwatch_bridge::mqtt::{self, Inbound};
use deskwatch_bridge::setup::Setup;
use deskwatch_bridge::source::unix_now;
use deskwatch_bridge::store::Store;
use deskwatch_bridge::supervisor::Supervisor;

/// How often the composer re-checks timers (alert expiry).
const COMPOSE_INTERVAL: Duration = Duration::from_secs(1);

const USAGE: &str = "\
usage: deskwatch-bridge [--demo] [--kiosk] [--no-mqtt] [--healthcheck] [CONFIG]
       deskwatch-bridge ado-capture --help
       deskwatch-bridge auth reset|set-password [CONFIG]   (the login, docs/auth.md)

CONFIG defaults to $DESKWATCH_CONFIG, then /etc/deskwatch/bridge.toml
(on Windows %ProgramData%\\DeskWatch\\bridge.toml).
--demo  play a loop of fake data instead of running the sources; the sources
        of CONFIG are ignored, and CONFIG is optional
--kiosk serve the browser dashboard even if [kiosk] enabled is not set
        (see docs/kiosk.md)
--no-mqtt  do not connect to a broker (same as [mqtt] enabled = false), for a
        bridge that only serves the dashboard page
--healthcheck  check that the bridge's HTTP listener answers on this machine
        (and, with MQTT on, that the broker connection is up), then exit 0
        (healthy) or 1; used by the Docker HEALTHCHECK

--demo, --kiosk and --no-mqtt can also be set with DESKWATCH_DEMO=1,
DESKWATCH_KIOSK=1 and DESKWATCH_NO_MQTT=1. A container's health check sees the
environment but not the command line, so set them there.";

/// Command line: `[--demo] [--kiosk] [--no-mqtt] [--healthcheck] [CONFIG]`.
struct Args {
    demo: bool,
    kiosk: bool,
    no_mqtt: bool,
    healthcheck: bool,
    config: Option<PathBuf>,
}

/// Is an on/off environment variable switched on (`1`, `true` or `yes`)?
fn env_flag(name: &str) -> bool {
    flag_value(std::env::var(name).ok().as_deref())
}

fn flag_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        demo: env_flag("DESKWATCH_DEMO"),
        kiosk: env_flag("DESKWATCH_KIOSK"),
        no_mqtt: env_flag("DESKWATCH_NO_MQTT"),
        healthcheck: false,
        config: None,
    };
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--demo") => args.demo = true,
            Some("--kiosk") => args.kiosk = true,
            Some("--no-mqtt") => args.no_mqtt = true,
            Some("--healthcheck") => args.healthcheck = true,
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            Some(flag) if flag.starts_with('-') => anyhow::bail!("unknown option {flag}\n{USAGE}"),
            _ if args.config.is_none() => args.config = Some(arg.into()),
            _ => anyhow::bail!("only one config path is allowed\n{USAGE}"),
        }
    }
    Ok(args)
}

/// Exit code after a restart asked for on the setup page.
const RESTART_EXIT_CODE: i32 = 75;

#[tokio::main]
async fn main() -> Result<()> {
    // `deskwatch-bridge ado-capture ...` records sanitised Azure DevOps
    // answers (docs/azure-devops-verify.md) and then exits; no config, no logs.
    let mut raw_args = std::env::args().skip(1);
    match raw_args.next().as_deref() {
        Some("ado-capture") => {
            return deskwatch_bridge::azure_devops::capture::run_cli(raw_args.collect()).await;
        }
        // The host-side password commands (docs/auth.md).
        Some("auth") => return deskwatch_bridge::auth::run_cli(raw_args.collect()),
        _ => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = parse_args()?;
    let mut config = match Config::path_given(args.config.clone()) {
        // The demo runs without a config file, against a broker on localhost.
        None if args.demo => Config::from_toml("")?,
        // The health probe also runs against a container started without a
        // config file (the demo); it then has nothing to probe unless told.
        // The same goes for the demo when the image's default config path
        // (its CMD) points at a file that is not mounted.
        given if (args.healthcheck || args.demo) && !Config::path(given.clone()).exists() => {
            Config::from_toml("")?
        }
        given => {
            let path = Config::path(given);
            let config = Config::load(&path)?;
            info!("loaded config from {}", path.display());
            config
        }
    };

    // The data folder (docs/data-dir.md): sources and secrets kept there are
    // added to the ones in bridge.toml. Off unless `[data] dir` or
    // DESKWATCH_DATA_DIR is set.
    let mut store = None;
    if let Some(dir) = config.data_dir() {
        let opened = Store::open(&dir)?;
        let overlay = opened.read_sources()?;
        config = config
            .with_overlay(&overlay)
            .with_context(|| format!("in {}", dir.join("sources.toml").display()))?;
        info!("data folder {}", dir.display());
        if config.mqtt_before_overlay.is_some() {
            info!("MQTT settings changed on the setup page are applied on top of bridge.toml");
        }
        store = Some(opened);
    }

    config.kiosk.enabled |= args.kiosk;
    config.mqtt.enabled &= !args.no_mqtt;

    if args.healthcheck {
        // The Docker HEALTHCHECK: no logging, just the exit code.
        match health::check(&config).await {
            Ok(_) => return Ok(()),
            Err(err) => {
                eprintln!("unhealthy: {err:#}");
                std::process::exit(1);
            }
        }
    }

    // Sources report here. The sender stays alive in this function, so `recv`
    // simply waits forever when no source is configured.
    let (source_tx, mut source_rx) = mpsc::channel(64);
    let mut hooks = Hooks::default();
    let supervisor = Supervisor::new(source_tx.clone(), hooks.webhooks());
    // Alerts reach the main loop here: from MQTT, and
    // from the kiosk page (dismiss) and the setup page (test alert). The
    // sender stays alive here either way, so with MQTT off the receiver just
    // waits forever.
    let (inbound_tx, mut inbound_rx) = mpsc::channel(16);
    // The daily "newer version" look (docs/updates.md). Not in the demo.
    let update = deskwatch_bridge::update::UpdateNotice::default();
    if config.updates.check && !args.demo {
        update.spawn(env!("CARGO_PKG_VERSION"));
    }
    // Setup and POST /api/alert only answer the local network unless
    // `[http] allow_public` (docs/setup-page.md, "Security").
    // The login (docs/auth.md): off until an administrator password exists.
    let auth = deskwatch_bridge::auth::Auth::open(
        config.data_dir().as_deref(),
        config.http.tls.is_some(),
    )?;
    auth.configure(config.auth.required && !args.demo, config.setup.is_on());
    if config.http.allow_public && !auth.enabled() {
        anyhow::bail!(
            "[http] allow_public needs a login password first: run `deskwatch-bridge auth set-password` \
             on the host (docs/auth.md), or remove the setting"
        );
    }
    if auth.enabled() {
        info!("login is on: the dashboard and setup need a sign-in");
        if config.http.tls.is_none() && !config.http.listen.ip().is_loopback() {
            warn!(
                "the login works over plain HTTP, but passwords and the session cookie then cross the network \
                 unprotected. Use [http.tls] or a TLS proxy before anyone outside a trusted network signs in"
            );
        }
    }
    let local_only = axum::middleware::from_fn_with_state(
        deskwatch_bridge::access::Access::new(config.http.allow_public)
            .with_login_flag(auth.enabled_flag())
            .with_allowed_hosts(config.http.allowed_hosts.clone()),
        deskwatch_bridge::access::require_local,
    );
    use deskwatch_bridge::auth::{Need, enforce};
    let guard = |need| axum::middleware::from_fn_with_state(auth.guard(need), enforce);
    hooks.add("/login", auth.router());
    // The pages' typefaces: no login, the login page needs them too.
    hooks.add("/fonts", deskwatch_bridge::fonts::router());
    // The tab icon: no login either, the login page shows it.
    hooks.add("/favicon.svg", deskwatch_bridge::icons::router());
    let mut kiosk = None;
    // Notices for colleagues (the Alerting Kiosk): typed on the setup page,
    // kept in the data folder.
    let notices = config
        .data_dir()
        .map(|dir| deskwatch_bridge::notices::Notices::load(&dir));
    if config.kiosk.enabled {
        if warn_no_login(auth.enabled(), config.http.listen.ip().is_loopback()) {
            warn!(
                "the kiosk page on {} has no login: anyone who can reach this port can read your PR titles, \
                 pipelines and host stats. That is fine on a home network. Do not expose the port to the internet, \
                 and do not publish it through a proxy or tunnel without a login on the proxy",
                config.http.listen
            );
        }
        let mut built = Kiosk::new(&config.kiosk);
        if let Some(dir) = config.data_dir() {
            built = built.with_data_dir(&dir);
        }
        if let Some(notices) = &notices {
            built = built.with_notices(notices.clone());
        }
        let built = built
            .with_setup_link(config.setup.is_on())
            .with_alerts(&config.alerts, inbound_tx.clone())
            .with_containers(supervisor.containers())
            .with_update(update.clone());
        hooks.add("/", built.router().layer(guard(Need::Viewer)));
        kiosk = Some(built);
    }
    // `/healthz` (the Docker health probe) reports the broker connection, so
    // the listener also opens for a bridge that only talks to a broker.
    let mqtt_link = health::MqttLink::default();
    // Alerts over HTTP: off unless `[alerts] http_input`.
    if config.alerts.enabled && config.alerts.http_input {
        hooks.add(
            deskwatch_bridge::alerts::HTTP_PATH,
            deskwatch_bridge::alerts::http_router(inbound_tx.clone())
                .layer(local_only.clone())
                .layer(guard(Need::AlertKey)),
        );
        hooks.add(
            deskwatch_bridge::alertmanager::HTTP_PATH,
            deskwatch_bridge::alertmanager::http_router(inbound_tx.clone())
                .layer(local_only.clone())
                .layer(guard(Need::AlertKey)),
        );
        if warn_no_login(auth.enabled(), config.http.listen.ip().is_loopback()) {
            warn!(
                "POST {} on {} has no login: anyone who can reach this port can put alerts on the dashboard. \
                 That is fine on a home network. Do not expose the port to the internet",
                deskwatch_bridge::alerts::HTTP_PATH,
                config.http.listen
            );
        }
    }
    // The setup page (docs/setup-page.md): off unless `[setup] enabled`.
    let mut setup = None;
    if config.setup.is_on() {
        let built = Setup::new(
            &config,
            mqtt_link.clone(),
            store.clone(),
            Some(supervisor.clone()),
        )
        .with_alert_sender(inbound_tx.clone())
        .with_update(update.clone())
        .with_notices(notices.clone());
        hooks.add(
            "/setup",
            built
                .router()
                .layer(local_only.clone())
                .layer(guard(Need::Admin)),
        );
        info!("setup page on /setup");
        if warn_no_login(auth.enabled(), config.http.listen.ip().is_loopback()) {
            warn!(
                "the setup page on {} has no login: anyone who can reach this port can read how the bridge is set up \
                 and, with a data folder, change sources and store tokens. Use it on a trusted network only; never \
                 expose the port to the internet or publish it through a proxy or tunnel without a login",
                config.http.listen
            );
        }
        setup = Some(built);
    }
    if config.mqtt.enabled {
        hooks.add("/healthz", mqtt_link.route());
    }
    // Build and start the sources. A missing credential or a bad setting
    // stops the bridge here, before it connects to the broker. The demo runs
    // none of them. Later changes from the setup page go through the same
    // supervisor, without a restart.
    if !args.demo {
        let outcome = supervisor.reconcile(&config, &[]).await;
        if let Some((id, reason)) = outcome.failed.first() {
            anyhow::bail!("source {id}: {reason}");
        }
    }
    hooks
        .start(config.http.listen, config.http.tls.as_ref())
        .await?;

    let publisher = if config.mqtt.enabled {
        let (publisher, eventloop) = mqtt::connect(&config.mqtt, mqtt::password(&config.mqtt)?)?;
        tokio::spawn(mqtt::run_event_loop(
            eventloop,
            publisher.clone(),
            inbound_tx.clone(),
            config.alerts.enabled,
            mqtt_link,
        ));
        Some(publisher)
    } else {
        info!("MQTT is off: serving the dashboard page only, no MQTT alerts");
        None
    };

    if config.server.enabled {
        warn!(
            "[server] enabled = true does nothing any more: the bridge no longer reads its own machine. Add it as a Prometheus host (docs/hosts.md)"
        );
    }
    let mut composer;
    let mut demo = None;
    if args.demo {
        info!("demo mode: playing fake data, sources are not started");
        composer = Composer::default();
        demo = Some(Demo::new(unix_now()));
    } else {
        composer = Composer::default();
        composer.alerts = Alerts::new(config.alerts.clone());
    }

    let mut compose_ticker = tokio::time::interval(COMPOSE_INTERVAL);
    let mut shutdown = Shutdown::new()?;
    // The setup page's restart button (only when `[setup] allow_restart`).
    let restart = setup.as_ref().map(Setup::restart_signal);
    let mut restarting = false;
    loop {
        tokio::select! {
            _ = compose_ticker.tick() => {}
            Some(msg) = source_rx.recv() => composer.apply(msg, unix_now()),
            Some(msg) = inbound_rx.recv() => match msg {
                Inbound::Alert(alert) => composer.alert(alert, unix_now()),
            },
            _ = shutdown.wait() => break,
            _ = async {
                match &restart {
                    Some(signal) => signal.notified().await,
                    None => std::future::pending().await,
                }
            } => {
                restarting = true;
                break;
            }
        }
        if let Some(demo) = &mut demo {
            demo.tick(&mut composer, unix_now());
        }
        if let Some(kiosk) = &mut kiosk {
            kiosk.update(&mut composer, unix_now());
        }
        if let Some(setup) = &setup {
            setup.update_health(
                composer
                    .health
                    .iter()
                    .map(|(id, health)| (id.to_string(), health.name())),
            );
            setup.update_reasons(
                composer
                    .health
                    .reasons()
                    .map(|(id, why)| (id.to_string(), why.to_string())),
            );
        }
    }

    info!("shutting down");
    if let Some(publisher) = &publisher {
        // Not forever: with the broker down the goodbye cannot be queued.
        match tokio::time::timeout(Duration::from_secs(2), publisher.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!("clean shutdown failed: {err:#}"),
            Err(_) => warn!("clean shutdown timed out"),
        }
        // Give the event loop a moment to flush the offline message and disconnect.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if restarting {
        // Not 0: systemd's Restart=on-failure and Docker's restart policies
        // both start the bridge again after this.
        info!("exiting so the supervisor starts the bridge again");
        std::process::exit(RESTART_EXIT_CODE);
    }
    Ok(())
}

/// Resolves when the process is asked to stop, so the bridge can say goodbye
/// to the broker. Ctrl+C everywhere; SIGTERM (systemd) on Unix; closing the
/// console window, logoff and system shutdown on Windows.
#[cfg(unix)]
struct Shutdown(tokio::signal::unix::Signal);

#[cfg(unix)]
impl Shutdown {
    fn new() -> Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self(signal(SignalKind::terminate())?))
    }

    async fn wait(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.0.recv() => {}
        }
    }
}

#[cfg(windows)]
struct Shutdown {
    close: tokio::signal::windows::CtrlClose,
    logoff: tokio::signal::windows::CtrlLogoff,
    shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl Shutdown {
    fn new() -> Result<Self> {
        use tokio::signal::windows;
        Ok(Self {
            close: windows::ctrl_close()?,
            logoff: windows::ctrl_logoff()?,
            shutdown: windows::ctrl_shutdown()?,
        })
    }

    async fn wait(&mut self) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = self.close.recv() => {}
            _ = self.logoff.recv() => {}
            _ = self.shutdown.recv() => {}
        }
    }
}

/// Whether the startup warnings "... has no login" apply: only while no
/// administrator password is set and the listener is reachable from the network.
fn warn_no_login(login_on: bool, loopback: bool) -> bool {
    !login_on && !loopback
}

#[cfg(test)]
mod tests {
    use super::{flag_value, warn_no_login};

    #[test]
    fn no_login_warning_only_without_a_password_on_the_network() {
        assert!(warn_no_login(false, false), "no password, reachable");
        assert!(!warn_no_login(true, false), "password set");
        assert!(!warn_no_login(false, true), "loopback only");
        assert!(!warn_no_login(true, true));
    }

    #[test]
    fn environment_flags_accept_the_usual_spellings() {
        for on in ["1", "true", "TRUE", "yes"] {
            assert!(flag_value(Some(on)), "{on}");
        }
        for off in ["", "0", "false", "no", "off"] {
            assert!(!flag_value(Some(off)), "{off}");
        }
        assert!(!flag_value(None));
    }
}
