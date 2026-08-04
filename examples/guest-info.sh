#!/bin/sh
set -eu

printf 'hostname=%s\n' "$(hostname)"
printf 'kernel=%s\n' "$(uname -r)"
printf 'uptime_seconds=%s\n' "$(cut -d. -f1 /proc/uptime)"
