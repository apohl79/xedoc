# Untrusted NAT bridge

```text
native app -- pinned host TLS + device certificate --> sendnix:50001
                                                       |
host:127.0.0.1:50001 <-- raw TCP <-- outbound frpc/mTLS --+
```

The relay forwards bytes; it never terminates the app's TLS. Pair the app
with the real host over a trusted out-of-band channel before using the relay.
The app generates its own identity in Keychain; no iOS/macOS trust profile is
needed. Pairing must bind the host certificate pin, bridge URL and device
enrollment permission. Ordinary browsers are not supported by this pairing flow.

There are two independent identities: the connector's transport certificate
admits a host to frps; the app's paired device certificate authorizes access at
the real Host GUI. Outer mTLS alone does **not** protect content from sendnix.
Only pinned, end-to-end host TLS and host-side device authorization do.
Do not configure an HTTP proxy, TLS-terminating plugin or proxy-protocol header.

## Transport certificates (trusted offline machine)

Use OpenSSL 3. Keep the signing key off sendnix. These certificates are only
for frp transport, not Host GUI TLS or device enrollment.

```sh
umask 077
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout bridge-ca.key -out bridge-ca.crt -days 3650 \
  -subj '/CN=Xedoc bridge transport CA' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign'
openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout server.key -out server.csr -subj '/CN=sendnix.de'
printf '%s\n' 'basicConstraints=critical,CA:FALSE' \
  'keyUsage=critical,digitalSignature' 'extendedKeyUsage=serverAuth' \
  'subjectAltName=DNS:sendnix.de' > server.ext
openssl x509 -req -in server.csr -CA bridge-ca.crt -CAkey bridge-ca.key \
  -CAcreateserial -out server.crt -days 90 -extfile server.ext
openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout host.key -out host.csr -subj '/CN=xedoc-host-1'
printf '%s\n' 'basicConstraints=critical,CA:FALSE' \
  'keyUsage=critical,digitalSignature' 'extendedKeyUsage=clientAuth' > host.ext
openssl x509 -req -in host.csr -CA bridge-ca.crt -CAkey bridge-ca.key \
  -CAcreateserial -out host.crt -days 90 -extfile host.ext
```

For additional hosts, issue separate clientAuth certificates and keys.
Possession of any admitted connector certificate permits claiming any free port
in the allowed range; this is not per-host port authorization. Endpoint pinning
prevents a substituted host from impersonating the paired host.

## Relay deployment

Copy `compose.yaml` and `frps.toml` to the deployment directory. Put **only**
`server.crt`, `server.key` and public `bridge-ca.crt` in its `relay/` subdirectory.
Never copy `bridge-ca.key`,
`host.key`, Host GUI TLS keys, app identity keys or pairing invites to sendnix.
Make config/certificates readable by container UID 65532; keep its key private:

```sh
chmod 644 frps.toml relay/server.crt relay/bridge-ca.crt
sudo chown 65532:65532 relay/server.key
sudo chmod 400 relay/server.key
docker compose up -d
```

Allow incoming TCP 7000 and the allocated ports 50001–50010 in the firewall.
There is no dashboard. The official image is pinned to its multi-platform
[upstream release image](https://github.com/fatedier/frp/blob/v0.72.0/.github/workflows/build-and-push-image.yml)
digest. Review/pin upgrades explicitly.

## NAT host

On macOS or Linux, provision the host's **own** transport certificate and key
and the public bridge CA through a trusted channel. The default paths are
`$XEDOC_HOME/bridge/transport/host.crt`, `host.key`, and `bridge-ca.crt`
(`XEDOC_HOME` defaults to `~/.xedoc`). Keep the transport CA signing key off the
host and relay. Do not reuse another host's private key.

Run setup on the host:

```sh
xedoc-remote-agentd device-pairing setup --bridge-origin https://sendnix.de:50002
```

Use `--transport-ca`, `--transport-cert`, and `--transport-key` for other
credential paths. Without `--bridge-origin`, setup keeps the host's configured
origin, or selects an apparently unused port for first-time setup.
An already-working route retains its existing connector instead of starting a
second one.

Setup installs upstream `frpc` v0.72.0 with pinned archive checksums, writes
the matching proxy configuration, activates the loopback-only Host GUI bridge,
and starts a persistent user connector (LaunchAgent on macOS, systemd user
service on Linux). It checks the public host certificate before reporting
ready. Missing credentials or a failed tunnel are an incomplete setup, not a
successful invitation. User services start at login; boot without login needs
separate administrator-managed daemon or systemd linger configuration.
The installer runs this setup for configured bridges and offers it as an
opt-in for first-time NAT access. Directly reachable hosts do not need it.
No inbound NAT port is required; the connector opens outbound TCP 7000.

Each public port is for one host, not shared by every host. In **File → Pair
Device…**, Xedoc checks `50001–50010` and suggests the first port that appears
unused. Use that port in the setup command; setup generates a unique proxy
name and matching local/remote ports. The check does not reserve a port: an
offline connector can look unused. If a configured route is unreachable,
rerun setup without changing its port first. Choose **Check again** after
setup reports ready, then create an invitation. The wizard verifies the
public route presents this host's certificate; it will not invite devices to
another host's route.

Bridge settings and host TLS material live under
`$XEDOC_HOME/bridge/host-gui/`; the bridge listener
requires paired-device certificates, while the direct listener retains account
login. Setup leaves the direct listener configuration unchanged. The private
bridge TLS key stays on the host, not sendnix.

Allocate a different port from the allowed range for each additional host,
changing the setup origin. The bridge port must differ
from the direct listener's port. Share pairing invites only over a trusted
channel, never a page served by sendnix.
To change an existing host's port on the same bridge hostname, rerun setup
with its new origin. Setup updates the listener and connector together.
Create fresh invitations for devices previously paired to the old port.

For subsequent invitations, run `xedoc-remote-agentd device-pairing invite` or
use **File → Pair Device…** on a trusted Mac. Add `--xedoc-home /path/to/home` if
needed. Transfer the
printed invitation directly to the macOS/iOS app's **Pair a host** dialog.
The invitation expires after ten minutes and works once; generate a separate
one for each device. The app creates a Keychain key and pins the host without
installing an OS profile. Existing account login remains required.
Use `xedoc-remote-agentd device-pairing list` and `device-pairing revoke <fingerprint>`
to remove access. Certificates expire after 90 days; pair again to renew.
Revocation blocks subsequent requests, not already-running streams.
Host TLS certificate replacement also requires a new trusted invitation.

The relay can still observe addresses, timing and volume, drop traffic, and
deny service. Protect both endpoints and the pairing channel. Rotate connector
certificates before expiry; removing a host requires replacing the relay's
trusted CA or another explicit revocation mechanism (none is configured here).
