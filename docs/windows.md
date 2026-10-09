# DeskWatch on Windows

DeskWatch supports **Linux and Windows**. The Linux guide is [install.md](install.md); this one covers
Windows 10 and 11 (and Windows Server 2019 or newer), x86_64 only.

**macOS is not covered yet.** Nothing in the bridge is knowingly Linux- or Windows-only apart from the local
server stats (see below), so it may well work, but nobody has written or tested the instructions. If you use a
Mac, a pull request with a `docs/macos.md` is very welcome.

> **How much of this is verified?** The bridge was compiled and linted for a Windows target on Linux, and the
> `windows` job in CI builds the whole workspace (including the simulator with a bundled SDL2) and runs the tests
> on a real Windows runner. Everything else here, the Mosquitto steps, the service and scheduled task setup, the
> firewall rule and the shutdown behavior, was written from documentation and **has not been run on Windows by
> the author**. If a step does not work for you, please open an issue with the error message.

What you need:

- Windows with PowerShell (the built-in Windows PowerShell 5.1 is fine; commands below also work in PowerShell 7).
- A Mosquitto broker the bridge can reach. For a first test, install it on the same PC.
- For the simulator: nothing extra when you use the release zip (SDL2 is built into `deskwatch-sim.exe`).

Contents: [1. Get the programs](#1-get-the-programs) · [2. Mosquitto](#2-mosquitto) ·
[3. Quick test with the demo](#3-quick-test-the-demo-and-the-simulator) · [4. A real config](#4-a-real-config) ·
[5. Run it as a service](#5-run-the-bridge-as-a-service) · [6. Panel mock flow](#6-the-panel-mock-flow) ·
[7. What differs from Linux](#7-what-differs-from-linux) · [8. Troubleshooting](#8-troubleshooting)

## 1. Get the programs

### Option A: download a release

Each release at <https://github.com/Sylentic/DeskWatch/releases> has `deskwatch-<version>-x86_64-windows.zip` and
a `.sha256` file next to it. The zip holds `deskwatch-bridge.exe`, `deskwatch-sim.exe`, the example config,
`deploy/`, `homeassistant/`, `docs/` and the licenses.

```powershell
$Version = "v0.9.7"      # the release you want
$Name = "deskwatch-$Version-x86_64-windows"
cd $env:TEMP
$Base = "https://github.com/Sylentic/DeskWatch/releases/download/$Version"
Invoke-WebRequest "$Base/$Name.zip" -OutFile "$Name.zip"
Invoke-WebRequest "$Base/$Name.zip.sha256" -OutFile "$Name.zip.sha256"

# The .sha256 file holds "<hash>  <file name>". Both lines must show the same hash.
(Get-Content "$Name.zip.sha256").Split(" ")[0]
(Get-FileHash "$Name.zip" -Algorithm SHA256).Hash.ToLower()

Expand-Archive "$Name.zip" -DestinationPath .
cd $Name
```

The exes are not code signed, so SmartScreen may warn on first run ("More info", then "Run anyway"). If you
downloaded the zip in a browser, right click it, choose Properties, and tick "Unblock" before extracting.

### Option B: build from source

You need [Rust](https://rustup.rs) 1.88 or newer with the MSVC toolchain, which rustup sets up with the Visual
Studio Build Tools ("Desktop development with C++"), plus [Git](https://git-scm.com) and
[CMake](https://cmake.org/download/) (the simulator builds SDL2 from source and needs it).

```powershell
git clone https://github.com/Sylentic/DeskWatch.git
cd DeskWatch
git checkout v0.9.7            # or stay on main for the newest, untagged code
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"    # needed with CMake 4 or newer, harmless otherwise
cargo build --release --locked -p deskwatch-bridge -p deskwatch-sim
```

The programs are `target\release\deskwatch-bridge.exe` and `target\release\deskwatch-sim.exe`. The first
simulator build takes a few minutes because of SDL2. If you only want the bridge, leave out `-p deskwatch-sim`;
then neither CMake nor SDL2 is needed.

## 2. Mosquitto

Install Mosquitto from <https://mosquitto.org/download/> (the Windows installer). It installs the "Mosquitto
Broker" service, but does **not** start it automatically, and the service may need a restart after you change its
config.

For a first test on one PC the defaults are enough: the broker listens on `localhost:1883` and, with no config
file, accepts anonymous clients from this machine only. Start it from an administrator PowerShell:

```powershell
Start-Service mosquitto
# or, to watch its log in the console instead of using the service:
& "C:\Program Files\mosquitto\mosquitto.exe" -v
```

To see what is on the broker, in another window:

```powershell
& "C:\Program Files\mosquitto\mosquitto_sub.exe" -h localhost -t "deskpanel/#" -v
```

For a broker other machines reach (your panel, Home Assistant), add logins and an ACL as described in
[mosquitto.md](mosquitto.md) and use the examples in [deploy/mosquitto](../deploy/mosquitto). On Windows the
config lives in `C:\Program Files\mosquitto\mosquitto.conf` and the password file is made with
`mosquitto_passwd.exe -c <file> <user>`. Windows Defender Firewall asks to allow Mosquitto on first start; allow
it only on the network type you trust, and never expose port 1883 to the internet.

## 3. Quick test: the demo and the simulator

This needs no Gitea, GitHub or Home Assistant. The demo plays a loop of fake data (see the README, "Demo mode")
and the simulator draws the panel in a window. Use two PowerShell windows, in the folder with the exes.

Window 1, the bridge in demo mode (a broker on localhost, as above):

```powershell
.\deskwatch-bridge.exe --demo
```

You should see `demo mode: playing fake data` and `connected to MQTT broker`. Leave it running.

Window 2, the simulator:

```powershell
.\deskwatch-sim.exe --host localhost
```

A 960x640 window opens (480x320 at scale 2; `--scale 1` makes it smaller) and walks through the pages: stats,
pull requests, pipelines, a running deploy, a failure, a green flash, alerts. Left click is the panel's button
(hold for 600 ms for a long press); space or enter is a short press, `L` a long press. Stop the bridge with
Ctrl+C; it tells the broker it is going offline, and the panel shows the red "bridge offline" bar.

The simulator also works without a broker or the bridge:

```powershell
# flip through the example payloads (click or space for the next one); needs the repository checkout
.\deskwatch-sim.exe preview ui\testdata\screens --badges ui\testdata\badges\default.json
# draw one payload to a PNG, no window
.\deskwatch-sim.exe render ui\testdata\screens\job_deploy.json -o job.png
```

(The example payloads are in the repository, not in the release zip.)

If the broker needs a login, run the simulator with `--user deskwatch-panel` and put the password in the
environment first: `$env:DESKWATCH_MQTT_PASSWORD = '...'` (this lasts for that PowerShell window only).

## 4. A real config

Keep the program, the config and the secrets in separate places:

| What | Where |
|---|---|
| Programs | `C:\Program Files\DeskWatch\` |
| Config | `C:\ProgramData\DeskWatch\bridge.toml` (the default path, like `/etc/deskwatch/bridge.toml`) |
| Secrets | `C:\ProgramData\DeskWatch\credentials\` (one file per secret, locked down, see below) |

From an **administrator** PowerShell in the extracted folder:

```powershell
New-Item -ItemType Directory -Force "C:\Program Files\DeskWatch", "C:\ProgramData\DeskWatch\credentials" | Out-Null
Copy-Item deskwatch-bridge.exe, deskwatch-sim.exe "C:\Program Files\DeskWatch\"
Copy-Item config.example.toml "C:\ProgramData\DeskWatch\bridge.toml"      # Option B: bridge\config.example.toml
```

Edit `bridge.toml` as described in [install.md, section 3](install.md#3-configure-the-bridge) and the comments in
the file; sources, rotation and alerts are the same on Windows. Two Windows details:

- In TOML, a backslash starts an escape in a normal string. Write Windows paths in single quotes
  (`ca_file = 'C:\ProgramData\DeskWatch\ca.pem'`) or with forward slashes (`"C:/ProgramData/DeskWatch/ca.pem"`).
- The webhook listener (`http.listen`, port 8787) is reachable from other machines only if Windows Defender
  Firewall allows it, see [section 5](#5-run-the-bridge-as-a-service).

### Secrets

There is no systemd `LoadCredential=` on Windows. The same config keys work (`token_file`, `password_file`,
`webhook_secret_file`), and a **plain name** is looked up in `C:\ProgramData\DeskWatch\credentials\` (or in the
folder named by the `CREDENTIALS_DIRECTORY` environment variable, if you set it). An absolute path also works.

Create each secret in its own file, then restrict the folder to administrators and the account that runs the
bridge (SYSTEM, if you follow the service steps below):

```powershell
$Cred = "C:\ProgramData\DeskWatch\credentials"
# Read the secret without echoing it, write it without a trailing newline problem (the bridge trims whitespace).
$Secret = Read-Host "Gitea webhook secret" -AsSecureString
[System.Net.NetworkCredential]::new("", $Secret).Password | Set-Content -NoNewline -Encoding ascii "$Cred\gitea-webhook-secret"

# Remove inherited permissions, then allow only SYSTEM and Administrators.
icacls $Cred /inheritance:r /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F"
```

and in `bridge.toml`: `webhook_secret_file = "gitea-webhook-secret"`. Generate a random webhook secret with
`-join ((1..32) | ForEach-Object { '{0:x2}' -f (Get-Random -Maximum 256) })`. As on Linux, secrets never go into
`bridge.toml`. The MQTT password also has the `DESKWATCH_MQTT_PASSWORD` environment variable as a fallback to
`password_file`.

Check the bridge reads the config, without a service, in the same administrator window:

```powershell
$env:RUST_LOG = "info"
& "C:\Program Files\DeskWatch\deskwatch-bridge.exe"
```

Look for `loaded config from C:\ProgramData\DeskWatch\bridge.toml` and `connected to MQTT broker`, then press
Ctrl+C.

## 5. Run the bridge as a service

Windows services need a wrapper because the bridge is an ordinary console program, not a native service. Two
options; pick one. Both run it as `SYSTEM` at boot, restart it after a crash, and need an administrator
PowerShell.

### Option A: NSSM (recommended)

[NSSM](https://nssm.cc) (the Non-Sucking Service Manager) is a small program that turns any exe into a service,
with log files and restart handling. Download it, put `nssm.exe` somewhere on your `PATH`, then:

```powershell
$Exe = "C:\Program Files\DeskWatch\deskwatch-bridge.exe"
New-Item -ItemType Directory -Force "C:\ProgramData\DeskWatch\logs" | Out-Null

nssm install DeskWatchBridge $Exe "C:\ProgramData\DeskWatch\bridge.toml"
nssm set DeskWatchBridge DisplayName "DeskWatch bridge"
nssm set DeskWatchBridge Start SERVICE_AUTO_START
nssm set DeskWatchBridge AppEnvironmentExtra "RUST_LOG=info"
nssm set DeskWatchBridge AppStdout "C:\ProgramData\DeskWatch\logs\bridge.log"
nssm set DeskWatchBridge AppStderr "C:\ProgramData\DeskWatch\logs\bridge.log"
nssm set DeskWatchBridge AppRotateFiles 1
nssm set DeskWatchBridge AppRotateBytes 5000000
nssm set DeskWatchBridge AppRestartDelay 5000
nssm start DeskWatchBridge
```

Check it: `nssm status DeskWatchBridge` and `Get-Content C:\ProgramData\DeskWatch\logs\bridge.log -Tail 20 -Wait`.
To pass the MQTT password as an environment variable instead of a credential file, add it to
`AppEnvironmentExtra` (`nssm set DeskWatchBridge AppEnvironmentExtra "RUST_LOG=info" "DESKWATCH_MQTT_PASSWORD=..."`),
but remember that service settings can be read by administrators; the credential file is the tidier choice.

Stop, update and remove:

```powershell
nssm stop DeskWatchBridge          # sends Ctrl+C first, so the bridge says goodbye to the broker
nssm restart DeskWatchBridge       # after changing bridge.toml or replacing the exe
nssm remove DeskWatchBridge confirm
```

### Option B: a scheduled task (nothing to download)

Task Scheduler is built in. It starts the bridge at boot and restarts it if it exits, but has no log rotation, so
the output is redirected to a file through `cmd.exe`:

```powershell
$Exe = "C:\Program Files\DeskWatch\deskwatch-bridge.exe"
$Cfg = "C:\ProgramData\DeskWatch\bridge.toml"
$Log = "C:\ProgramData\DeskWatch\logs\bridge.log"
New-Item -ItemType Directory -Force (Split-Path $Log) | Out-Null

$Action = New-ScheduledTaskAction -Execute "cmd.exe" `
    -Argument "/c `"`"$Exe`" `"$Cfg`" >> `"$Log`" 2>&1`""
$Trigger = New-ScheduledTaskTrigger -AtStartup
$Settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName "DeskWatchBridge" -Action $Action -Trigger $Trigger -Settings $Settings `
    -User "SYSTEM" -RunLevel Highest

Start-ScheduledTask -TaskName DeskWatchBridge
```

Stopping a task ends the process without a Ctrl+C, so the broker only learns the bridge is gone from its Last
Will (the panel still shows "bridge offline", just a moment later):

```powershell
Stop-ScheduledTask -TaskName DeskWatchBridge
Unregister-ScheduledTask -TaskName DeskWatchBridge -Confirm:$false     # remove
```

### Webhook port

Only needed if Gitea (or another sender) is on a different machine than the bridge:

```powershell
New-NetFirewallRule -DisplayName "DeskWatch webhooks" -Direction Inbound -Protocol TCP -LocalPort 8787 `
    -Action Allow -Profile Private -RemoteAddress LocalSubnet
```

Adjust the port to `http.listen`, and the remote address to where Gitea runs. Webhooks are plain HTTP, so keep
this to a trusted network (see the README).

## 6. The panel mock flow

The real panel (ESP32-S3 firmware) is not built yet, so on Windows the simulator stands in for it. To rehearse
the whole chain with your real bridge, not the demo:

1. Run the bridge with your config (as in section 4, or as the service) against your broker.
2. Start the simulator against the same broker: `deskwatch-sim.exe --host broker.example.lan --user deskwatch-panel`
   (password in `$env:DESKWATCH_MQTT_PASSWORD`). The first screen appears as soon as the retained
   `deskpanel/screen` message arrives; the stats page then updates every few seconds.
3. Trigger something: run a Gitea Actions workflow for a job page, or raise an alert from Home Assistant or by
   hand:

   ```powershell
   & "C:\Program Files\mosquitto\mosquitto_pub.exe" -h localhost -t deskpanel/alert -f docs\schema\alert-raise.json
   ```

   Press the simulator's button (click) to dismiss a failed alert or step through pages, as on the real panel.
4. Draw a payload yourself, before the bridge makes one, to see how the panel handles it:

   ```powershell
   & "C:\Program Files\mosquitto\mosquitto_pub.exe" -h localhost -r -t deskpanel/screen -f ui\testdata\screens\alert_failed.json
   ```

   (Retained, so it stays until the bridge publishes the next screen.)

## 7. What differs from Linux

- **Local server stats.** The stats page of the `[server]` source reads `/proc` and `/sys`, which Windows does not
  have. On Windows the bridge runs fine, but it knows only the machine name (from `COMPUTERNAME`, or
  `server.display_name`); CPU, load, memory, disk, uptime and network show as unknown (dashes). To see a Windows
  PC's numbers, run [windows_exporter](https://github.com/prometheus-community/windows_exporter) on it and add a
  Prometheus source; the bundled queries are written for node_exporter, so which Windows figures appear depends
  on the metric names, and that mapping is not done yet. Real local stats for Windows (CPU, memory, disk through
  the Windows API) are planned work.
- **Source guides.** [azure-devops.md](azure-devops.md) and the other source docs show Linux commands for the token
  files; on Windows create the same file in `C:\ProgramData\DeskWatch\credentials\` as in section 4 and use the
  plain name in the config.
- **Credentials.** A folder of locked-down files instead of systemd `LoadCredential=` (section 4).
- **Service.** NSSM or a scheduled task instead of systemd (section 5).
- **Shutdown.** Ctrl+C, closing the console window, logoff and system shutdown all make the bridge disconnect
  from the broker cleanly. SIGTERM does not exist on Windows.
- **Config path.** `C:\ProgramData\DeskWatch\bridge.toml` is the default, or pass a path, or set
  `DESKWATCH_CONFIG`.

Everything else (sources, rotation, alerts, MQTT schema, the panel pages) is the same on both systems.

## 8. Troubleshooting

- **`cannot read config file ...` on start.** The default config path is under `C:\ProgramData`, not next to the
  exe. Pass the path as the first argument, or set `$env:DESKWATCH_CONFIG`.
- **`cannot read credential <name> at ...`.** The file is not in `C:\ProgramData\DeskWatch\credentials\`, or the
  account running the bridge cannot read it (check `icacls` on the folder). The message shows the full path tried.
- **The bridge exits at once as a service, nothing in the log.** Run the same command in an administrator
  PowerShell; start-up errors (port in use, missing credential, bad TOML) print to the console. In TOML,
  backslashes in double-quoted strings are escapes, use single quotes.
- **`connection refused` to the broker.** Mosquitto is not running (`Get-Service mosquitto`), or listens on
  another port. By default it only accepts connections from this PC.
- **Windows Defender Firewall blocks the broker or the webhook port.** See sections 2 and 5.
- **The simulator window shows nothing.** No `deskpanel/screen` message has
  arrived: check the bridge is running and both use the same broker, host, port and `--prefix`. Use
  `mosquitto_sub.exe -t "deskpanel/#" -v` to see what is on the broker.
- **The simulator window is too large or small.** `--scale 1` or `--scale 3`.
- **SmartScreen blocks the exes.** See the note in section 1.
