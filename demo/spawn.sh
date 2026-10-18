#!/usr/bin/env bash

set -e

IMAGES=(
  alpine
  debian:bookworm-slim
  nginx:stable-alpine3.21-slim
)

COUNT_PER_IMAGE=5

echo "Starting containers..."

for image in "${IMAGES[@]}"; do
  safe_name=$(echo "$image" | tr '/:.' '_')

  for ((i=1; i<=COUNT_PER_IMAGE; i++)); do
    name="tui_${safe_name}_${i}"

    echo "Launching $name from $image"

    docker run -d \
      --name "$name" \
      --restart unless-stopped \
      "$image" \
      sh -c '
        services="api worker db cache auth scheduler metrics gateway"
        infos="request completed connection established heartbeat received task processed cache refreshed sync completed"
        warns="high memory usage retrying request slow response detected queue backlog detected"
        errors="database timeout upstream unavailable disk write failure authentication failure"

        rand_item() {
          list="$1"
          set -- $list
          eval "echo \${$((RANDOM % $# + 1))}"
        }

        while true; do
          ts=$(date -Iseconds)

          r=$((RANDOM % 100))

          if [ "$r" -lt 85 ]; then
            level="INFO"
            msg=$(rand_item "$infos")
          elif [ "$r" -lt 97 ]; then
            level="WARNING"
            msg=$(rand_item "$warns")
          else
            level="ERROR"
            msg=$(rand_item "$errors")
          fi

          service=$(rand_item "$services")
          req=$((RANDOM % 90000 + 10000))
          latency=$((RANDOM % 500))

          echo "$ts level=$level service=$service request_id=req-$req latency_ms=$latency msg=\"$msg\""

          sleep $((RANDOM % 3 + 1))
        done
      ' >/dev/null
  done
done

echo
echo "Done."
echo "Total containers:"
docker ps --filter "name=tui_" --format "{{.Names}}"
