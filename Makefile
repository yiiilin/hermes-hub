.PHONY: test backend-test frontend-test dev-db hermes-image

test: backend-test frontend-test

backend-test:
	cargo test --workspace

frontend-test:
	cd frontend && npm test

dev-db:
	docker compose --env-file deploy/.env.example -f deploy/compose.yml up -d postgres

hermes-image:
	docker build -f docker/hermes/Dockerfile -t ghcr.io/yiiilin/hermes-hub-hermes:latest .
