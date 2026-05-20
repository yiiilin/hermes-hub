.PHONY: test backend-test frontend-test dev-db

test: backend-test frontend-test

backend-test:
	cargo test --workspace

frontend-test:
	cd frontend && npm test

dev-db:
	docker compose --project-directory . -f infra/docker/docker-compose.yml up -d postgres
