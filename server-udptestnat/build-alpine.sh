#!/usr/bin/env bash
# 海外网络被禁的服务器专用：不进 GitHub 拉代码、不拉 docker 镜像，
# 直接用本机已有的 lejianwen/rustdesk-server-s6（Alpine 3.22, musl）容器原生构建，
# 产物与运行环境同 libc，天然二进制兼容。
# 用法（在服务器上）：cd /opt/hbbs-udp-punch && bash build-alpine.sh
set -euo pipefail

BASE="$(cd "$(dirname "$0")" && pwd)"
IMG="${IMG:-lejianwen/rustdesk-server-s6:latest}"

cd "$BASE"
[ -f hbbs-src.tar.gz ] || { echo "缺少 hbbs-src.tar.gz（从本地上传）"; exit 1; }
[ -f gitcache.tar.gz ] || { echo "缺少 gitcache.tar.gz（GitHub 依赖缓存）"; exit 1; }
docker image inspect "$IMG" >/dev/null 2>&1 || { echo "本机没有镜像 $IMG"; exit 1; }

[ -d work/rustdesk-server ] || { mkdir -p work && tar xzf hbbs-src.tar.gz -C work; }
# gitcache.tar.gz 顶层就是 <repo>-<hash> 目录，必须落在 cargo-home/git/db/ 下
if [ ! -d work/cargo-home/git/db/async-speed-limit-498ba1f6b5a19d20 ]; then
  mkdir -p work/cargo-home/git/db
  tar xzf gitcache.tar.gz -C work/cargo-home/git/db
fi
echo "==> 校验 Cargo.lock 锁定的 git 修订全部命中缓存"
cd work
missing=0
for rev in $(grep -A3 'source = "git+' rustdesk-server/Cargo.lock | grep -oE '[0-9a-f]{40}' | sort -u); do
  hit=0
  for d in cargo-home/git/db/*/; do
    git --git-dir="$d" cat-file -t "$rev" >/dev/null 2>&1 && { hit=1; break; }
  done
  [ "$hit" = 0 ] && { echo "MISS $rev"; missing=1; }
done
[ "$missing" = 0 ] || { echo "git 缓存不完整，中止（否则容器内会去连 GitHub）"; exit 1; }
echo "==> git 依赖缓存校验通过"
cd "$BASE"
# 补丁内容校验（源码包应已含补丁，这里双保险）
grep -q "is_udp: socket.is_some()" work/rustdesk-server/src/rendezvous_server.rs
grep -q "Answer UDP TestNat"       work/rustdesk-server/src/rendezvous_server.rs
grep -q "udp_port: ph.udp_port"    work/rustdesk-server/src/rendezvous_server.rs
grep -q "IPv6 introduction was Pro-server-only" work/rustdesk-server/src/rendezvous_server.rs
grep -q "ph.socket_addr_v6"                     work/rustdesk-server/src/rendezvous_server.rs
grep -q "phs.socket_addr_v6.clone()"               work/rustdesk-server/src/rendezvous_server.rs

mkdir -p work/rustdesk-server/out work/rustup
# Alpine 镜像没有 bash，用 sh（脚本为 POSIX 写法）；服务器容器网桥 DNS 不通，用 host 网络
docker run --rm --entrypoint sh --network host \
  -v "$BASE/work/rustdesk-server:/io" \
  -v "$BASE/work/cargo-home:/root/.cargo" \
  -v "$BASE/work/rustup:/root/.rustup" \
  -v "$BASE/build-container.sh:/build-container.sh:ro" \
  -w /io "$IMG" /build-container.sh

OUT="$BASE/hbbs-udp-punch"
cp work/rustdesk-server/target/release/hbbs "$OUT"
chmod +x "$OUT"
ls -la "$OUT"; sha256sum "$OUT"
echo "==> 完成：$OUT，按 README 里 2.x 步替换进容器"
