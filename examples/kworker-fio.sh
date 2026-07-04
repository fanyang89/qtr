#!/bin/sh
set -eu

count_kworkers() {
  count=0
  for comm in /proc/[0-9]*/comm; do
    [ -r "$comm" ] || continue
    name=$(cat "$comm" 2>/dev/null || true)
    case "$name" in
      kworker/*) count=$((count + 1)) ;;
    esac
  done
  printf '%s\n' "$count"
}

baseline=$(count_kworkers)
max=$baseline
samples=0
sum=0

fio --name=qtr \
  --filename=/dev/vdb \
  --direct=1 \
  --rw=randread \
  --bs=4k \
  --iodepth=32 \
  --numjobs=4 \
  --runtime=60 \
  --time_based \
  --group_reporting \
  --output-format=json > /tmp/qtr-fio.json &
fio_pid=$!

while kill -0 "$fio_pid" 2>/dev/null; do
  current=$(count_kworkers)
  [ "$current" -gt "$max" ] && max=$current
  samples=$((samples + 1))
  sum=$((sum + current))
  sleep 0.5
done

wait "$fio_pid"

if [ "$samples" -eq 0 ]; then
  avg=$baseline
else
  avg=$((sum / samples))
fi

printf '{"kworker":{"baseline":%s,"max":%s,"avg":%s,"samples":%s},"fio":' \
  "$baseline" "$max" "$avg" "$samples"
cat /tmp/qtr-fio.json
printf '}\n'
