# Deployment

qtr can be packaged as an RPM containing the server binary, built Web UI, systemd integration, service user declaration, and persistent directory layout.

## Build the RPM

Building requires the Rust and Web UI toolchains, Task, and nFPM. Build the package with:

```bash
task package:rpm
```

The generated RPM is written to `dist/`. The installed service listens on `127.0.0.1:8080` and expects its API token at `/etc/qtr/api-token`.

## Configure the Service

After installing the RPM, create the service token and start qtr:

```bash
sudo systemctl enable --now libvirtd
QTR_TOKEN="$(openssl rand -hex 32)"
printf '%s\n' "$QTR_TOKEN" | sudo tee /etc/qtr/api-token >/dev/null
sudo chown root:qtr /etc/qtr/api-token
sudo chmod 0640 /etc/qtr/api-token
sudo systemctl enable --now qtr
```

Open `/access` through a TLS reverse proxy and enter `QTR_TOKEN`. The proxy must support WebSocket upgrades for the VNC console. qtr remains loopback-only by default; do not expose port 8080 directly.

## Deploy with Ansible

Deploy the RPM to Fedora 44 hosts with the uv-managed Ansible playbook:

```bash
uv sync
cp deploy/inventory.example.ini deploy/inventory.ini
task deploy:rpm LIMIT=fedora44
```

The deploy playbook copies the local RPM, installs it with `dnf5`, creates the API token when absent, enables `libvirtd` and qtr, then waits for the health endpoint.
