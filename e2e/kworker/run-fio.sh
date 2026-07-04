#!/bin/sh
set -eu

fio_dir=/mnt/data/db

print_kworker_count() {
  label=$1
  count=$(ps -e -o comm= | awk '/^kworker/ { count++ } END { print count + 0 }')
  printf '%s kworker count: %s\n' "$label" "$count"
}

mkdir -p "$fio_dir"
mkdir -p "$fio_dir/direct"
mkdir -p "$fio_dir/redo"
mkdir -p "$fio_dir/buffered"

print_kworker_count before

fio \
  --name=xfs_buffered_writers \
  --directory="$fio_dir/buffered" \
  --rw=write \
  --bs=16k \
  --numjobs=64 \
  --size=1G \
  --ioengine=sync \
  --direct=0 \
  --fdatasync=512 \
  --runtime=600 \
  --time_based \
  --group_reporting

print_kworker_count after
