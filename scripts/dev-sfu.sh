#!/usr/bin/env bash
# Runs a Telium SFU for Rhein development, built from the clone in .dev/telium/sfu, in front of
# the livekit-server in .dev/bin (which it spawns). It trusts both dev servers:
#
#   scripts/dev-sfu.sh                     SFU on http://127.0.0.1:7890, LiveKit on :7880
#   RHEIN_SFU=1 scripts/dev-server.sh a    a dev server whose voice region "local" is this SFU
#
# State (the generated livekit.yaml, keys) lives in .dev/sfu/.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
clone="$root/.dev/telium"
export CARGO_TARGET_DIR="$root/.dev/target"
bin="$CARGO_TARGET_DIR/debug/telium-sfu"
livekit="$root/.dev/bin/livekit-server"

if [[ ! -x "$livekit" ]]; then
  echo "no livekit-server in .dev/bin (see crates/rhein-voice/tests/livekit_spike.rs)" >&2
  exit 1
fi
if [[ ! -x "$bin" ]]; then
  (cd "$clone/sfu" && cargo build)
fi

dir="$root/.dev/sfu"
mkdir -p "$dir"
cd "$dir"
cat > sfu.toml <<TOML
bind = "127.0.0.1:7890"

[livekit]
mode = "Spawn"
url = "http://127.0.0.1:7880"
public_url = "ws://127.0.0.1:7880"
api_key = ""
api_secret = ""
binary = "$livekit"
config_dir = "."

[livekit.rtc]
port_range_start = 50000
port_range_end = 50200
tcp_port = 7881
use_external_ip = false

[livekit.turn]
enabled = false
udp_port = 3478

[livekit.room]
empty_timeout = 60
max_participants = 0

[trust]
discovery = true
cache_ttl_secs = 600
public = false
allow_insecure_discovery = false

[[trust.origins]]
server_name = "localhost"
discovery_url = "http://127.0.0.1:8448"

[[trust.origins]]
server_name = "b.localhost"
discovery_url = "http://127.0.0.1:8449"

[admin]
enabled = false
bind = "127.0.0.1:7891"
state_dir = "./state"
helper_path = "/usr/local/libexec/telium-sfu-admin-helper"

[limits]
max_bitrate = 8000000
enforce = true
poll_interval_secs = 15
TOML
exec "$bin" serve --config sfu.toml
