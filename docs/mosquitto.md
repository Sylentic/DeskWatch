# Mosquitto logins and topic limits

DeskWatch works with or without broker logins, but a broker that accepts clients without a login lets anything
on the network publish `deskpanel/screen` and draw on the panel. This guide gives the bridge, the panel and
Home Assistant each their own login, limited to their own topics. All names, paths and hosts below are
placeholders.

Files in this repo:

| File | Purpose |
|---|---|
| [`deploy/mosquitto/mosquitto.conf.example`](../deploy/mosquitto/mosquitto.conf.example) | Broker settings: logins required, password file, ACL file |
| [`deploy/mosquitto/acl.example`](../deploy/mosquitto/acl.example) | One user per client, each limited to its own topics |

Mosquitto 2.x is assumed. The commands and paths are for a Linux broker (systemd, `/etc/mosquitto`); on Windows
the broker's config and password file live under `C:\Program Files\mosquitto`, see
[windows.md](windows.md#2-mosquitto), and the bridge's password goes in a credential file as described in
[windows.md](windows.md#secrets). The ACL and the topic rules are the same everywhere.

## 1. Who may do what

| User | Reads | Writes |
|---|---|---|
| `deskwatch-bridge` | `deskpanel/#` | `deskpanel/#` |
| `deskwatch-panel` | `deskpanel/screen`, `deskpanel/badges`, `deskpanel/bridge/status` | `deskpanel/panel/status`, `deskpanel/panel/event` |
| `homeassistant` | nothing under `deskpanel/` | `deskpanel/alert` |

So Home Assistant (or a script with the same rule) can raise and clear alerts, but cannot write
`deskpanel/screen`, and a stolen panel login cannot raise alerts.

## 2. Migration: check who uses the broker first

Turning anonymous access off disconnects every client that has no login, including things unrelated to
DeskWatch (sensors, Zigbee or Tasmota bridges, Home Assistant itself, scripts). Find them first.

1. **Log connections.** Set `connection_messages true` and reload (`systemctl reload mosquitto`, or
   `docker kill -s HUP <container>`). Each connect then logs a line such as
   `New client connected from 192.0.2.10:51234 as some-client-id (p2, c1, k60, u'name')`. The address and client id
   tell you what it is. No `u'...'` part means it connected anonymously.
2. **Count and watch live.** `mosquitto_sub -h <broker> -t '$SYS/broker/clients/#' -v` shows connected and total
   clients (the broker publishes `$SYS` topics by default).
3. **Cross-check on the host.** `ss -tn state established '( sport = :1883 )'` lists remote addresses with open
   connections; map each address to a device or service.
4. **Wait a day or two.** Some clients only connect now and then (a backup job, a battery sensor). Keep the log
   from that period and list every client id and address you saw.
5. **Give each client a login.** Add a user and ACL lines for everything on that list (copy the pattern in
   `acl.example`), and put the credentials into that client's settings.

### Switching over without a cliff

Mosquitto can run two listeners with different rules, so you can move clients one at a time:

```
per_listener_settings true

# Old port: unchanged until the last client has moved.
listener 1883
allow_anonymous true

# New port: logins and ACLs.
listener 1884
allow_anonymous false
password_file /etc/mosquitto/passwd
acl_file /etc/mosquitto/acl
```

Point each client at port 1884 with its login, check that it works, and when nothing connects to 1883 any more
(check the log as in step 1), delete the first listener. Then change 1884 back to 1883 if you want the
usual port, and update the clients' port once more. If you prefer one step, edit the existing listener instead and
keep the old `mosquitto.conf` around so you can roll back.

## 3. Create the logins

`mosquitto_passwd` asks for the password twice; use long random ones (`openssl rand -base64 24`). `-c` creates the
file and overwrites an existing one, so use it only for the first user.

```sh
sudo mosquitto_passwd -c /etc/mosquitto/passwd deskwatch-bridge
sudo mosquitto_passwd /etc/mosquitto/passwd deskwatch-panel
sudo mosquitto_passwd /etc/mosquitto/passwd homeassistant
sudo chown mosquitto: /etc/mosquitto/passwd && sudo chmod 600 /etc/mosquitto/passwd
```

Then install the ACL from `acl.example` at the path in `acl_file`, add your other clients' users, and reload the
broker. Mosquitto rereads the password and ACL files on a reload signal (SIGHUP) without dropping connections;
changes to `allow_anonymous` or listeners need a restart.

## 4. Bridge settings

The bridge reads the password from a systemd credential, never from the config file:

```toml
[mqtt]
host = "localhost"
username = "deskwatch-bridge"
password_file = "mqtt-password"
```

```sh
sudo install -d -m 700 /etc/deskwatch/credentials
sudo sh -c 'umask 077; printf %s "THE-BRIDGE-PASSWORD" > /etc/deskwatch/credentials/mqtt-password'
```

On Linux, uncomment `LoadCredential=mqtt-password:...` in `deploy/deskwatch-bridge.service`, then
`sudo systemctl daemon-reload && sudo systemctl restart deskwatch-bridge`. Without `password_file` the bridge
falls back to the `DESKWATCH_MQTT_PASSWORD` environment variable, which is handy for testing only. The log line
`connected to MQTT broker` confirms the login worked; a wrong login shows `MQTT connection error` and retries
every 5 seconds.

The panel's login goes into the firmware settings, see [firmware/README.md](../firmware/README.md). The Home Assistant login goes into its
MQTT integration, see [home-assistant.md](home-assistant.md).

## 5. TLS (optional)

Logins travel in clear text on a plain listener, which is acceptable on a trusted wired network and not on shared
Wi-Fi. For TLS, add a listener with `cafile`, `certfile` and `keyfile` (commented example in
`mosquitto.conf.example`), then in `bridge.toml`:

```toml
[mqtt]
port = 8883
tls = true
ca_file = "/etc/deskwatch/mqtt-ca.pem"   # only for a private CA; omit for a public one
```

The certificate must name the host the bridge connects to. If the broker asks for client certificates
(`require_certificate true`), also set `client_cert_file` and `client_key_file` (a credential name, loaded like
the password). The Home Assistant side has its own TLS option in the MQTT integration.

## 6. Check it

```sh
# No login: refused (in Mosquitto 2.x the connection is rejected).
mosquitto_pub -h <broker> -t deskpanel/alert -m '{}'
# Home Assistant's login may write the alert topic ...
mosquitto_pub -h <broker> -u homeassistant -P '...' -t deskpanel/alert \
  -m '{"v":2,"id":"test","severity":"info","title":"Test"}'
# ... but a write to the screen topic is dropped silently (publish is accepted, nobody receives it).
mosquitto_pub -h <broker> -u homeassistant -P '...' -t deskpanel/screen -m '{}'
```

Mosquitto drops ACL-denied publishes without telling the publisher at QoS 0, so confirm with
`mosquitto_sub -u deskwatch-bridge -P '...' -t 'deskpanel/#' -v` in another terminal: the alert shows up, the
screen message does not.
