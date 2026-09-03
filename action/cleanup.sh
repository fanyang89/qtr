#!/usr/bin/env bash
set -u

pid=${STATE_qtr_pid:-}
service_dir=${STATE_service_dir:-}
service_log=${STATE_service_log:-}
install_dir=${STATE_install_dir:-}
qtr_path=${STATE_qtr_path:-}

if [[ "$pid" =~ ^[0-9]+$ ]] && kill -0 "$pid" 2>/dev/null; then
    running_executable=$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)
    expected_executable=$(readlink -f "$qtr_path" 2>/dev/null || true)
    if [[ -n "$expected_executable" && "$running_executable" == "$expected_executable" ]]; then
        kill -TERM "$pid" 2>/dev/null || true
        for _ in {1..30}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 1
        done
        if kill -0 "$pid" 2>/dev/null; then
            printf '::warning::qtr did not stop within 30 seconds; sending SIGKILL\n' >&2
            kill -KILL "$pid" 2>/dev/null || true
        fi
    else
        printf '::warning::not stopping PID %s because it is no longer the qtr process\n' "$pid" >&2
    fi
fi

if [[ -n "$service_log" && -f "$service_log" ]]; then
    printf '::group::qtr final service log\n'
    tail -n 100 "$service_log" || true
    printf '::endgroup::\n'
fi

for directory in "$service_dir" "$install_dir"; do
    if [[ -n "$directory" && "$directory" == "${RUNNER_TEMP:-/__unset__}"/* ]]; then
        rm -rf -- "$directory" || true
    fi
done

exit 0
