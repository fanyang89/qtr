# qtr Documentation

## Start Here

- [Getting started](getting-started.md): prepare a Fedora host and run qtr locally.
- [Web UI and API](web-api.md): operate the authenticated server and managed resources.
- [VM configuration](vm-configuration.md): define and manage virtual machines with YAML and the CLI.
- [Direct VMs](direct-vm.md): run experimental Cloud Hypervisor VMs without libvirt.

## Operations

- [Automated Fedora installation](fedora-installation.md): install Fedora Server from a local DVD ISO.
- [External storage](external-storage.md): discover and connect iSCSI-backed volumes.
- [Deployment](deployment.md): package qtr as an RPM and deploy it with systemd or Ansible.
- [GitHub Actions](github-actions.md): start an ephemeral qtr service on an Ubuntu CI runner.
- [Server-managed runs](managed-runs.md): recover ephemeral resources by persistent run identity.
- [Jepsen smoke test](jepsen-smoke.md): validate three real nodes, lifecycle faults, and rollback.
- [Cloud guest E2E](cloud-vm-e2e.md): validate real guest readiness, reboot, and cleanup.
- [Jepsen adapter](../integrations/jepsen/README.md): provision ephemeral nodes and inject hypervisor lifecycle faults.

## Reference

- [OpenAPI 3.1 document](../openapi/qtr-v1.json)
- [Web UI development](../web/README.md)
- [CLI help](getting-started.md#cli-help)
