#!/bin/sh
set -eu

fio_dir=/mnt/data/db
fio_file=$fio_dir/direct/unwritten.dat
fio_file_size=${FIO_FILE_SIZE:-64G}
fio_job_size=${FIO_JOB_SIZE:-1G}
fio_runtime=${FIO_RUNTIME:-600}
fio_numjobs=${FIO_NUMJOBS:-64}
fio_iodepth=${FIO_IODEPTH:-64}
fio_bs=${FIO_BS:-16k}

print_kworker_count() {
  label=$1
  count=$(ps -e -o comm= | awk '/^kworker/ { count++ } END { print count + 0 }')
  printf '%s kworker count: %s\n' "$label" "$count"
}

mkdir -p "$fio_dir"
mkdir -p "$fio_dir/direct"

rm -f "$fio_file"
fallocate -l "$fio_file_size" "$fio_file"

samples_file=/tmp/qtr-kworker.samples
rm -f "$samples_file"

print_kworker_count before

(
  while :; do
    ps -e -o comm= | awk '/^kworker/ { count++ } END { print count + 0 }'
    sleep 1
  done
) >"$samples_file" &
sampler_pid=$!

set +e
fio \
  --name=xfs_direct_unwritten_extents \
  --filename="$fio_file" \
  --rw=randwrite \
  --bs="$fio_bs" \
  --numjobs="$fio_numjobs" \
  --size="$fio_job_size" \
  --offset_increment="$fio_job_size" \
  --ioengine=libaio \
  --iodepth="$fio_iodepth" \
  --direct=1 \
  --allow_file_create=0 \
  --fallocate=none \
  --runtime="$fio_runtime" \
  --time_based \
  --group_reporting
fio_status=$?
set -e

kill "$sampler_pid" 2>/dev/null || true
wait "$sampler_pid" 2>/dev/null || true

print_kworker_count after

awk '
  NR == 1 { min = max = $1 }
  { sum += $1; samples++; if ($1 < min) min = $1; if ($1 > max) max = $1 }
  END {
    if (samples == 0) {
      print "kworker samples: 0"
    } else {
      printf "kworker samples: %d min: %d max: %d avg: %.2f\n", samples, min, max, sum / samples
    }
  }
' "$samples_file"

exit "$fio_status"
