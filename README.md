# Hermes Hub

Hermes Hub is an invite-only multi-user control plane for isolated Hermes agent instances.

## Development

```bash
make dev-db
cargo test --workspace
cd frontend && npm install && npm test
```

The project is implemented from the approved design in `docs/superpowers/specs/2026-05-19-hermes-hub-design.md`.
