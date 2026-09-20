# hbbs UDP 打洞补丁 — 部署与验证

给自建的 `lejianwen/rustdesk-server-s6`（forapi 分支 hbbs）补上开源版缺失的 UDP 打洞三件套，
让 PC ↔ 手机流量/平板在外网场景下能走真正的 UDP P2P（KCP），而不是永远中继。

## 补丁内容（基于 forapi commit `fb8b5b9`，子模块 hbb_common `d6b1497`）

只动 `src/rendezvous_server.rs`，五处纯增量：

1. **UDP TestNat 应答**（`handle_udp` 新 arm）：以前开源 hbbs 对 UDP 的 TestNatRequest 直接丢弃，
   客户端 `udp_nat_port` 永远是 0，UDP 打洞整条链路根本不会启动。现在仿照 TCP arm 回
   `TestNatResponse{port=观测到的源端口}`（+ 可选 ConfigUpdate）。
2. **`udp_port` 透传**（`handle_punch_hole_request`）：A 的 UDP 观测端口随 `PunchHole` 转发给 B，
   B 端客户端（已具备该逻辑）据此 `peer_addr.set_port(udp_port)` 并走 `punch_udp_hole`。
3. **`is_udp` + 正确回投**（`handle_hole_sent`）：B 的 PunchHoleSent 经 UDP 到达时，观测到的
   `addr` 正是 B 的 UDP 公网映射 → 回给 A 的 `PunchHoleResponse` 带 `is_udp=true`；且同时经
   `send_to_tcp` 走 A 发起请求的那条 TCP 连接投递（原来只从 UDP 发给 A 的 TCP 观测地址，是死信）。
4. **`socket_addr_v6` 透传给 B**（`handle_punch_hole_request`）：客户端 `start_ipv6` 的门控只读
   `PunchHole.socket_addr_v6` 这个专用字段（`socket_addr` 里塞 v6 地址不算数），而开源 hbbs
   （forapi 与官方 master 都一样）从不填它——IPv6 介绍是 Pro 服务端私有功能。现在优先转发 A 在
   `PunchHoleRequest.socket_addr_v6` 里**自报**的 v6 UDP 打洞 socket（B 会精确拨向该端口；观测到的
   TCP 源端口是一次性连接端口，UDP 上无人监听，不能作为目标），仅当 A 未自报且经 v6 连入时回退用
   观测地址，B 端才会去拨 A 的 IPv6。
5. **B 的 v6 回传给 A**（`handle_hole_sent`）：B 的 `start_ipv6` 会把自己公网 v6 放进
   `PunchHoleSent.socket_addr_v6` 返回；原先服务端构造 `PunchHoleResponse` 时直接丢弃。现在透传
   该字段（B 未报告时回退用观测地址的 v6），A 端 IPv6 监听/拨号才具备对端地址。

客户端（本仓库构建的 PC 版、以及 1.4+ 官方移动端）本来就齐了，缺的只有服务端。
前提是两端 hbbs 连接至少有一端走 v6 且双方都有公网 GUA（fn 节点 host 网络后已满足；江苏机暂无服务端 v6/AAAA）。

## 一、构建

### 路线 A：服务器禁海外网（江苏机实际采用）

这台机器（CentOS Stream 9，GitHub 不通）实测结论：
- docker.io 直连、GitHub、`swr.cn-north-4.../library/rust`（未同步）、
  `docker.m.daocloud.io`（blob 后端在海外）全部不可用；
- **rsproxy.cn、mirrors.aliyun.com 可达**；
- 现成镜像 `lejianwen/rustdesk-server-s6:latest` 基座是 **Alpine 3.22（musl）**，
  直接 `apk add rust cargo` 原生构建，产物与运行环境同 libc，无需再拉任何镜像；
- 容器网桥 DNS 不通，构建容器需 `--network host`；
- 8 个 GitHub git 依赖（async-speed-limit、rustdesk-org 的 reqwest/confy/tokio-socks/
  sysinfo/default_net/machine-uid）服务器上拿不到，已从本机能联网的机器把
  `~/.cargo/git/db` 里对应 7 个仓库打包成 `gitcache.tar.gz`（5.4MB）随源码一起上传。

准备（Windows 本机，能上外网）：`hbbs-src.tar.gz` = 打补丁后的完整 rustdesk-server 树
（去 .git/target），`gitcache.tar.gz` = 上述 git 依赖 db 目录打包。然后：

```bash
scp hbbs-src.tar.gz gitcache.tar.gz build-alpine.sh build-container.sh \
    root@<server>:/opt/hbbs-udp-punch/
```

服务器上：

```bash
cd /opt/hbbs-udp-punch
bash build-alpine.sh    # 日志在 build.log；容器内 apk(aliyun 源) 装 rust/cargo/build-base，
                        # crates.io 走 rsproxy，git 依赖全部命中本地缓存
# 产物 ./hbbs-udp-punch（x86_64-alpine-linux-musl，静态）
```

重复构建不用重新上传（源码解包在 `work/`，cargo 增量缓存也在里面）。

### 路线 B：服务器可达 GitHub（家里的 NAS 等）

```bash
cd /opt/hbbs-udp-punch
bash build-hbbs.sh            # 钉死 commit 克隆 + docker 内 musl 交叉构建，约 10~20 分钟
```

- 脚本按 `uname -m` 自动选 `x86_64/aarch64-unknown-linux-musl`（armv7 请改用官方 CI 交叉产物）。
- GitHub 慢就设 `GIT_PROXY=https://ghproxy.net/`；海外机器 `MIRROR=0 bash build-hbbs.sh` 关 rsproxy。
- 编译如果报依赖类错误，可试 `RUST_IMAGE=rust:1.84-bookworm bash build-hbbs.sh`。

## 二、替换运行中的 hbbs

```bash
CONT=$(docker ps --format '{{.Names}}' | grep -i rustdesk | head -1)
docker tag lejianwen/rustdesk-server-s6:latest lejianwen/rustdesk-server-s6:pristine-20260920  # 回滚锚点
docker exec $CONT cp /usr/bin/hbbs /usr/bin/hbbs.orig   # 容器内备份
docker cp /opt/hbbs-udp-punch/hbbs-udp-punch $CONT:/usr/bin/hbbs
docker restart $CONT
docker logs --tail 50 $CONT    # 应看到 hbbs Start，且无 udp failure
docker commit $CONT lejianwen/rustdesk-server-s6:latest # 固化进镜像（重建容器也不丢）
```

> **坑（2026-09-20 实际踩到）**：若宿主的 iptables `DOCKER` nat 链被清空过（firewalld reload 等），
> `docker restart` 会报 `Unable to enable DNAT rule ... No chain/target/match by that name`，
> 容器会以"只有 lo、无端口映射"的孤岛状态继续跑（健康检查也显示 healthy，极具迷惑性）：
> 现象 = 客户端 UDP 能进 tcpdump 但 hbbs 永远不回。修法：
> `systemctl restart docker`（重建 DOCKER 链）→ `docker network connect <net> <cont>`
> （把孤岛容器接回网桥并补全 DNAT 规则）。验证：
> `iptables -t nat -S DOCKER | grep 21116` 应有 tcp+udp 两条。

## 三、验证 UDP 打洞是否成功

1. **先确认 NAT 探测活了**（PC 端日志 `%APPDATA%\RustDesk\log\rustdesk.log`）：

   ```powershell
   Select-String 'UDP NAT test' $env:APPDATA\RustDesk\log\rustdesk.log | Select-Object -Last 5
   ```

   补丁前恒为 `success=false`；生效应看到 `port=<非零端口> ... success=true`。

2. **跨网直连实测**：手机关 WiFi 用 5G（或找非同网段的设备）连 PC / 互相连：

   客户端日志应出现
   `#1 UDP+TCP punch attempt with ...` → `UDP+TCP Hole Punched <id> = <对端公网IP:端口>` →
   建立连接，界面右上连接信息不再显示"中继"。
   对比旧日志只有 `TCP punch attempt`。

3. **失败排查顺序**：
   - 仍 `success=false` → UDP 21116 没通到容器（`docker logs` 里看 hbbs 是否绑了 udp；防火墙/1Panel 放行 UDP）。
   - 有 `UDP+TCP punch attempt` 但超时回落中继 → 大概率真对称 NAT（客户端会主动放弃 UDP 走中继，
     `SYMMETRIC` 判定可在 NAT type 日志里看），或运营商封了终端间 UDP。
   - `docker logs` 又见 `udp failure: Invalid argument` + 反复 Start → WS `X-Real-IP` 的 `ip:0`
     污染复发，先清注册（restart 容器）再查 21118 反代。

## 四、边界与回滚

- 本补丁解决**公网 UDP P2P**；同局域网互访仍走 LAN TCP 直连老路径（打洞对同网段本来就无效，hairpin），
  **小米手机的局域网 TCP 直连失败与本补丁无关**，仍待 adb logcat 定位。
- TestNat UDP 应答是"收到谁的回给谁"，无放大风险；不新增端口。
- 回滚：`docker exec $CONT cp /usr/bin/hbbs.orig /usr/bin/hbbs && docker restart $CONT`；
  若已 `docker commit` 固化，直接把镜像指回 `lejianwen/rustdesk-server-s6:pristine-20260920`
  （江苏机上已打好该 tag，即未打补丁的原始镜像）。

## 文件清单

| 文件 | 用途 |
| --- | --- |
| `hbbs-udp-punch.patch` | git diff（fb8b5b9 之上，仅 rendezvous_server.rs，+27/-1） |
| `rendezvous_server.patched.rs` | 打完整补丁的整文件（脚本用它覆盖，免 patch 工具） |
| `build-hbbs.sh` | 服务器侧一键构建（clone 钉死 commit + 覆盖文件 + docker 内 musl 构建） |
| `build-inside-container.sh` | 构建容器内部脚本（rsproxy 镜像、rustup target、cargo build） |
| `build-alpine.sh` | 路线 A 一键构建（禁海外网服务器：本地解包 + 缓存预检 + Alpine 容器原生编译） |
| `build-container.sh` | 路线 A 容器内脚本（apk 依赖 + rsproxy rustup + cargo build --release） |
