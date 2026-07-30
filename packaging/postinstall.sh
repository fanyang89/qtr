#!/bin/sh
set -eu

systemd-sysusers /usr/lib/sysusers.d/qtr.conf
systemd-tmpfiles --create /usr/lib/tmpfiles.d/qtr.conf
systemctl daemon-reload || :
systemctl preset qtr.service || :
