#!/usr/bin/env bash
# Runs a throwaway Telium homeserver for Rhein development, built from the clone in
# .dev/telium (never from ~/projects/telium itself).
#
#   scripts/dev-server.sh a     server_name "localhost",   http://localhost:8448
#   scripts/dev-server.sh b     server_name "b.localhost", http://localhost:8449
#   RHEIN_SFU=1 scripts/dev-server.sh a     the same with the dev SFU as its voice region
#
# Both instances federate with each other over plain HTTP through `federation.peers`, so the
# second one can host guilds for accounts whose home is the first. Sign-in rate limits are raised
# so the live tests and the demo seed can register accounts back to back.
#
# State lives in .dev/servers/<name>/ and survives restarts; delete that folder to start over.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
name="${1:-a}"
case "$name" in
  a) server_name="localhost"; port=8448 ;;
  b) server_name="b.localhost"; port=8449 ;;
  *) echo "usage: $0 a|b" >&2; exit 2 ;;
esac
clone="$root/.dev/telium"
export CARGO_TARGET_DIR="$root/.dev/target"
bin="$CARGO_TARGET_DIR/debug/telium-server"

if [[ ! -d "$clone" ]]; then
  git clone -q "$HOME/projects/telium" "$clone"
fi
if [[ ! -x "$bin" ]]; then
  (cd "$clone/server" && cargo build)
fi

# With RHEIN_SFU=1 the voice region "local" is the SFU of scripts/dev-sfu.sh; without it there
# is no region and calls are meshes. (A region that is down is still handed out when it is the
# only one, so it is not left configured when no SFU runs.)
if [[ "${RHEIN_SFU:-}" == 1 ]]; then
  regions='[{ name = "local", sfu = "http://127.0.0.1:7890" }]'
else
  regions='[]'
fi

dir="$root/.dev/servers/$name"
mkdir -p "$dir"
cd "$dir"
if [[ ! -f telium.toml ]]; then
  "$bin" -c telium.toml init
fi
sed -i \
  -e "s|^server_name = .*|server_name = \"$server_name\"|" \
  -e "s|^public_base_url = .*|public_base_url = \"http://localhost:$port\"|" \
  -e "s|^bind = .*|bind = \"127.0.0.1:$port\"|" \
  -e "/^\[federation\]/,/^\[/{s|^allow_http = .*|allow_http = true|}" \
  -e "/^\[rate_limit\]/,/^\[/{s|^auth_burst = .*|auth_burst = 1000|;s|^auth_per_sec = .*|auth_per_sec = 100.0|}" \
  -e "/^\[voice\]/,/^\[/{s|^regions = .*|regions = $regions|}" \
  telium.toml
# Where each logical server actually answers; a domain is an identity, not a hostname.
python3 - telium.toml <<'EOF'
import re, sys
path = sys.argv[1]
text = open(path).read()
peers = '[federation.peers]\n"localhost" = "http://localhost:8448"\n"b.localhost" = "http://localhost:8449"\n'
text = re.sub(r'\[federation\.peers\]\n(?:[^\[\n].*\n|\n)*?(?=\[|\Z)', peers + '\n', text, count=1)
open(path, 'w').write(text)
EOF
exec "$bin" -c telium.toml serve
