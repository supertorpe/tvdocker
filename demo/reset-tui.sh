#!/usr/bin/env bash
# Pre-flight for a tvdocker demo take:
#   1. wipe every tui_* container (state, stopped containers and any files the
#      Explorer act created) and respawn the full 15 from the bundled spawn.sh;
#   2. restore the seeded config (a take writes the picked theme and the shell
#      form's values into it).
# Keeps repeated takes idempotent. Path-independent: run it from anywhere.
set -e
here=$(cd "$(dirname "$0")" && pwd)

ids=$(docker ps -aq --filter name=tui_ || true)
if [ -n "$ids" ]; then
  docker rm -f $ids >/dev/null
fi
"$here/spawn.sh" >/dev/null

cfg="$here/record-home/.config/tvdocker"
cp "$cfg/tvdocker.toml.seed" "$cfg/tvdocker.toml"
echo "tui_* running: $(docker ps --filter name=tui_ -q | wc -l) / total: $(docker ps -aq --filter name=tui_ | wc -l)"
