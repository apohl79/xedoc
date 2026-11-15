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

Install upstream `frpc` v0.72.0 for the host OS from the
[official release](https://github.com/fatedier/frp/releases/tag/v0.72.0).
Keep `host.key` private on that host. Copy `frpc.example.toml`, replace the three
absolute certificate paths, and run `frpc -c /absolute/path/frpc.toml`.
No inbound NAT port is required; the connector opens outbound TCP 7000.

Keep the normal Host GUI listener and account login unchanged. On the trusted
host, configure a **separate**, loopback-only TLS listener for the relay:

```sh
xedoc-remote-agentd device-pairing invite --bridge-origin https://sendnix.de:50001
```

`50001` is an example, not a shared port for every host. In **File → Pair
Device…**, Xedoc checks `50001–50010` and suggests the first port that appears
unused. Use that port in the command and in the connector's `localPort` and
`remotePort`, with a unique proxy name such as `xedoc-host-50002`. The check
does not reserve the port: an offline connector can look unused. Confirm frpc
reports `start proxy success`, then choose **Check again** before creating an
invitation. The wizard also verifies the public route presents this host's
certificate; it will not invite devices to another host's route.

Restart Host GUI **before** starting the connector. This creates bridge settings
and host TLS material under `$XEDOC_HOME/bridge/host-gui/`; the bridge listener
requires paired-device certificates, while the direct listener retains account
login. First-time bridge setup also removes legacy app-managed certificate
enforcement from the direct listener; it never removes a custom client CA.
The private bridge TLS key stays on the host, not sendnix.

Allocate a different port from the allowed range for each additional host,
changing the invitation origin and both frpc ports. The bridge port must differ
from the direct listener's port. Share pairing invites only over a trusted
channel, never a page served by sendnix.
To change an existing host's port on the same bridge hostname, rerun the command
with its new origin, update its connector, and restart both Host GUI and frpc.
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
