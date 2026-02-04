# PS5Upload App (Remote Browser Mode)

`app/` runs PS5Upload in browser mode by serving the **same desktop React UI** (`desktop/dist`) with a web bridge.

## Important behavior

In browser mode, host-side file/folder paths are from the machine running `make run-app` (the server), not from the remote viewer device.

## Requirements

- Node.js 20+ (Node.js 22 recommended)
- npm

## Run

From repo root:

```bash
make run-app
```

This command prepares dependencies, builds desktop UI assets, then starts the app server.

## Host/Port customization

CLI flags:

```bash
cd app
npm start -- --host 0.0.0.0 --port 10331
```

Environment variables:

```bash
APP_HOST=0.0.0.0 APP_PORT=10331 make run-app
```


Defaults:

- Host: `0.0.0.0`
- Port: `10331`

## APIs used by web bridge

- `POST /api/invoke`
- `GET /api/health`
- `GET /api/config`
- `GET /api/network/interfaces`

## Notes

- Desktop (`desktop/`) remains fully supported.
- Web mode currently focuses on shared UI + storage/config flows; transport parity is incremental.
- Keep Awake in web mode now targets the host machine running the app service (not the remote browser device).
- Web mode browse dialogs use a server-host file browser modal (works on Windows/macOS/Linux). Paths are always resolved on the host running `make run-app`.
