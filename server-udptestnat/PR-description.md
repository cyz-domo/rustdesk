# PR draft — target: lejianwen/rustdesk-server, branch `forapi`

Prerequisite: fork https://github.com/lejianwen/rustdesk-server on GitHub (no
`cyz-domo/rustdesk-server` exists yet), then from the local clone
`C:/Users/R7000P-2021/AppData/Local/Temp/rdsrv-repo`:

```bash
git remote add myfork https://github.com/cyz-domo/rustdesk-server.git
https_proxy=http://127.0.0.1:7890 git push myfork hbbs-udp-ipv6
# then open PR: myfork:hbbs-udp-ipv6 -> lejianwen:forapi
```

The branch contains exactly two commits touching only `src/rendezvous_server.rs`
(+53 / −2). Apply to any forapi checkout with `git am 0001-*.patch 0002-*.patch`.

---

## Title

feat(rendezvous): enable UDP + IPv6 hole punching in open-source hbbs

## Body

### Problem

On a deployment with an open-source hbbs (this repo's `forapi` branch) and stock
clients (PC 1.5.x + official Android APK), UDP hole punching and IPv6 direct
connections never engage — every cross-NAT session falls back to relay. Three
server-side gaps cause this; the client side is already complete:

1. **UDP `TestNatRequest` is dropped.** `handle_udp` has no arm for it, so
   clients never learn their UDP mapping. `PunchHoleRequest.udp_port` stays 0
   and the UDP punch path in `Client::start` never runs.
2. **`udp_port` / `is_udp` are never relayed.** `PunchHole` does not carry A's
   observed UDP port to B, and `PunchHoleResponse` built from a UDP-arriving
   `PunchHoleSent` does not set `is_udp`, so A never switches to its UDP socket.
   Worse: when B replies over UDP, the response is sent to A's *TCP-observed*
   address over UDP — a dead letter.
3. **`socket_addr_v6` is never filled.** The client gates the whole IPv6 leg on
   the dedicated `PunchHole.socket_addr_v6` field (a v6 address inside
   `socket_addr` is ignored), and it *dials exactly that port*. Open-source hbbs
   leaves the field empty, so IPv6 introduction is effectively Pro-server-only.
   Note the port must be A's **self-reported** IPv6 UDP punch socket
   (`PunchHoleRequest.socket_addr_v6`); the TCP source port we observe is a
   connected-socket port nothing listens on for UDP, so reusing the observed
   address cannot land a v6 punch.

### Changes

- `feat(rendezvous): enable UDP hole punching in open-source hbbs` — answer UDP
  `TestNatRequest` with the observed source port (+ optional `ConfigUpdate`),
  mirroring the TCP arm; forward `udp_port` in `PunchHole`; set `is_udp` on
  `PunchHoleResponse` and deliver it over UDP **and** via A's TCP punch
  connection.
- `feat(rendezvous): pass through socket_addr_v6 to enable IPv6 hole punching` —
  forward A's self-reported v6 punch socket into `PunchHole.socket_addr_v6`
  (fallback: observed v6 addr when no self-report); carry B's
  `PunchHoleSent.socket_addr_v6` into `PunchHoleResponse.socket_addr_v6`.

No proto, API, or behavior changes for v4/TCP users; every change fills a field
the client already knows how to consume.

### Verification (live, 2026-09-20)

Environment: home PC (Windows, behind NAT, GUA available) + Xiaomi phone on
5G (official 1.4/1.5 APK, phone itself tests SYMMETRIC for UDP), patched hbbs
on both a NAS node and a cloud host.

- Before: `udp_nat_port` always 0; every remote session via relay.
- UDP TestNat loopback probe against patched hbbs returns
  `TestNatResponse{port}` where stock hbbs was silent.
- After: 5G phone ↔ PC establishes **direct IPv6 KCP in 85–102 ms**
  (`connect!()` + KCP latency logs), relay fallback never reached.

Happy to retest against any feedback.
