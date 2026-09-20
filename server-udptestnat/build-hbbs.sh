#!/usr/bin/env bash
# 在服务器上构建打过 UDP 打洞补丁的 hbbs（lejianwen/rustdesk-server forapi）。
# 用法：把 server-udptestnat/ 整个目录上传到服务器（如 /opt/hbbs-udp-punch/），
#       然后 cd 进该目录执行 bash build-hbbs.sh
# 依赖：docker（构建在容器里完成，宿主机不需要 rust）
set -euo pipefail

BASE_COMMIT=fb8b5b9cf494d045f0feb370d83913b1a7506388       # forapi HEAD 2026-09
SUBMODULE_COMMIT=d6b14975ffd35ed63528617a53795c479a6eaf13  # libs/hbb_common
RUST_IMAGE="${RUST_IMAGE:-rust:1-bookworm}"
# GitHub 直连慢/不通时设 GIT_PROXY=https://ghproxy.net/ （注意带尾部斜杠）
GIT_PROXY="${GIT_PROXY:-}"
# 国内服务器默认走 rsproxy.cn 镜像；海外服务器设 MIRROR=0 关闭
MIRROR="${MIRROR:-1}"

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK="$SRC_DIR/build/rustdesk-server"

arch=$(uname -m)
case "$arch" in
  x86_64)  TARGET=x86_64-unknown-linux-musl ;;
  aarch64) TARGET=aarch64-unknown-linux-musl ;;
  armv7l)  TARGET=armv7-unknown-linux-musleabihf ;;
  *) echo "不支持的架构: $arch"; exit 1 ;;
esac
echo "==> 目标三元组: $TARGET (arch=$arch)"

GIT_CFG=()
if [ -n "$GIT_PROXY" ]; then
  GIT_CFG=(-c "url.${GIT_PROXY}https://github.com/.insteadOf=https://github.com/")
fi

if [ ! -d "$WORK/.git" ]; then
  echo "==> 克隆 lejianwen/rustdesk-server @ $BASE_COMMIT (forapi)"
  git "${GIT_CFG[@]}" clone --depth 50 -b forapi https://github.com/lejianwen/rustdesk-server "$WORK"
else
  echo "==> 已存在 $WORK，复用"
fi
git -C "$WORK" "${GIT_CFG[@]}" fetch --depth 50 origin forapi 2>/dev/null || true
git -C "$WORK" "${GIT_CFG[@]}" checkout -f "$BASE_COMMIT"
git -C "$WORK" submodule update --init --recursive
git -C "$WORK/libs/hbb_common" checkout "$SUBMODULE_COMMIT"

echo "==> 覆盖打洞补丁后的 rendezvous_server.rs"
cp "$SRC_DIR/rendezvous_server.patched.rs" "$WORK/src/rendezvous_server.rs"
grep -q "is_udp: socket.is_some()" "$WORK/src/rendezvous_server.rs" \
  && grep -q "Answer UDP TestNat" "$WORK/src/rendezvous_server.rs" \
  && grep -q "udp_port: ph.udp_port" "$WORK/src/rendezvous_server.rs" \
  && grep -q "IPv6 introduction was Pro-server-only" "$WORK/src/rendezvous_server.rs" \
  && grep -q "ph.socket_addr_v6"                     "$WORK/src/rendezvous_server.rs" \
  && grep -q "phs.socket_addr_v6.clone()" "$WORK/src/rendezvous_server.rs" \
  || { echo "补丁内容校验失败"; exit 1; }

echo "==> 容器内 musl 静态构建（产物可跑在 alpine/debian/scratch 任意镜像里）"
docker run --rm \
  -v "$WORK:/io" -v "$SRC_DIR/build-inside-container.sh:/build-inside-container.sh:ro" \
  -w /io -e TARGET="$TARGET" -e MIRROR="$MIRROR" \
  "$RUST_IMAGE" bash /build-inside-container.sh

OUT="$SRC_DIR/hbbs-udp-punch"
cp "$WORK/target/$TARGET/release/hbbs" "$OUT"
chmod +x "$OUT"
ls -la "$OUT"
file "$OUT" 2>/dev/null || true
echo
echo "==> 构建完成: $OUT"
echo "    下一步按 README-deploy.md 替换容器里的 /usr/bin/hbbs 并重启。"
