.PHONY: test backend-test frontend-test dev-db hermes-image

test: backend-test frontend-test

backend-test:
	cargo test --workspace

frontend-test:
	cd frontend && npm test

dev-db:
	docker compose -f deploy/compose.dev.yml up -d postgres

hermes-image:
	docker compose -f deploy/compose.dev.yml --profile hermes-runtime build hermes-runtime
