# P2P WebSocket CDN Transport

RabbitChain supports two independently switchable P2P bootnode endpoint forms:

- `enode://peer@ip:port` for the existing direct TCP transport.
- `ws://...` or `wss://...` for WebSocket P2P transport behind an HTTP/CDN reverse proxy.

The WebSocket transport is intended for Cloudflare orange-cloud proxying. Cloudflare handles the
public HTTPS/WebSocket endpoint, then forwards the upgraded connection to an origin WebSocket P2P
listener. It does not cache P2P traffic and does not require Workers or Durable Objects.

## Transport Modes

Direct TCP is enabled by default. WebSocket outbound bootnodes are enabled by default, while the
WebSocket listener starts only when `--p2p-ws-listen-port` is set.

```bash
# TCP only
rabbitchain run --disable-p2p-ws

# WebSocket only
rabbitchain run \
  --disable-p2p-tcp \
  --p2p-ws-listen-addr 127.0.0.1 \
  --p2p-ws-listen-port 30304 \
  --bootnode wss://boot1.rabbitchain.org/p2p

# TCP and WebSocket together
rabbitchain run \
  --p2p-listen-addr 0.0.0.0 \
  --p2p-listen-port 30303 \
  --p2p-ws-listen-addr 127.0.0.1 \
  --p2p-ws-listen-port 30304
```

When direct TCP is disabled, discovery is not started because the current discovery path is IP/UDP
ENR-based and advertises direct TCP endpoints.

## Bootnode Origin

Run the node with both direct TCP P2P and a local WebSocket P2P listener:

```bash
rabbitchain run \
  --p2p-listen-addr 0.0.0.0 \
  --p2p-listen-port 30303 \
  --p2p-ws-listen-addr 127.0.0.1 \
  --p2p-ws-listen-port 30304 \
  --p2p-ws-external-url wss://boot1.rabbitchain.org/p2p
```

The `mainnet.sh` wrapper exposes the same flags:

```bash
scripts/mainnet.sh start bootnode \
  --p2p-listen-addr 0.0.0.0 \
  --p2p-ws-listen-addr 127.0.0.1 \
  --p2p-ws-listen-port 30304 \
  --p2p-ws-external-url wss://boot1.rabbitchain.org/p2p
```

Use `--disable-p2p-tcp` or `--disable-p2p-ws` with `mainnet.sh` to run only one transport.

For Cloudflare CDN mode, expose only HTTPS through a reverse proxy and firewall the origin P2P
WebSocket port to localhost.

Example Nginx origin route:

```nginx
location /p2p {
    proxy_pass http://127.0.0.1:30304;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
}
```

## Peer Connection

Peers can connect through Cloudflare:

```bash
rabbitchain run \
  --bootnode wss://boot1.rabbitchain.org/p2p \
  --disable-discovery
```

Peers can also keep direct TCP bootnodes and WebSocket bootnodes in the same config by repeating
`--bootnode`.

`--disable-discovery` is recommended for CDN bootnodes because the existing discovery layer is
IP/UDP ENR-based. Static WebSocket bootnodes are the supported CDN path.

## Operational Notes

- Keep direct TCP enabled for private/sentry peers when needed.
- Use WebSocket bootnodes as boot/sentry entry points, not as a single relay for all network gossip.
- Keep node-side P2P rate limits enabled; CDN WebSocket proxying does not replace protocol-level
  DoS controls.
- Protect the origin with firewall rules so peers only see the Cloudflare address.
- When traffic comes through a reverse proxy, the node's P2P per-IP limits apply to the proxy/origin
  connection address; do not make protocol decisions from untrusted forwarded headers.
