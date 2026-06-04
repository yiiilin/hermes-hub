.PHONY: test backend-test frontend-test dev-db hermes-image release

test: backend-test frontend-test

backend-test:
	cargo test --workspace

frontend-test:
	cd frontend && npm test

dev-db:
	docker compose --env-file deploy/.env.example -f deploy/compose.yml up -d postgres

hermes-image:
	docker build -f docker/hermes/Dockerfile -t ghcr.io/yiiilin/hermes-hub-hermes:latest .

release:
	@test -n "$(VERSION)" || (echo "用法: make release VERSION=0.0.23 NOTES='发版内容'"; exit 1)
	@if [ -n "$(NOTES_FILE)" ]; then \
		./scripts/release.sh "$(VERSION)" --notes-file "$(NOTES_FILE)"; \
	else \
		test -n "$(NOTES)" || (echo "用法: make release VERSION=$(VERSION) NOTES='发版内容'"; exit 1); \
		./scripts/release.sh "$(VERSION)" "$(NOTES)"; \
	fi
