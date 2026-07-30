# qtr Web UI

Admin dashboard UI for qtr, built from `satnaing/shadcn-admin` and trimmed for QEMU/libvirt VM control.

## Features

- shadcn/ui admin shell
- Responsive sidebar layout
- Dark/light theme
- TanStack Router routes
- TanStack Query API hooks
- VM dashboard and list pages
- Managed disk creation and ISO lifecycle pages
- noVNC console shell
- Vite `/api` proxy for the Rust web server

## Development

Install dependencies:

```bash
pnpm install
```

Run the Vite dev server:

```bash
pnpm dev
```

Build production assets:

```bash
pnpm build
```

The dev server proxies `/api` and WebSocket traffic to `http://127.0.0.1:8080`.
Set the Bearer token on `/access`; it is kept in `sessionStorage` and sent to `/api/v1` endpoints. `/settings` rotates or clears the current tab token. noVNC obtains a short-lived ticket before opening its WebSocket.

## Routes

- `/`: qtr dashboard
- `/vms`: VM list
- `/vms/$name`: VM detail
- `/vms/$name/console`: noVNC console
- `/installations`: automated Fedora installations
- `/resources/disks`: managed raw and qcow2 disk lifecycle
- `/resources/isos`: managed installation media
- `/settings`: host and web settings

## Attribution

This UI was initialized from [`satnaing/shadcn-admin`](https://github.com/satnaing/shadcn-admin), which is licensed under MIT. The copied template license is kept in `LICENSE`.
