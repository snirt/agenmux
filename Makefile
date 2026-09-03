.PHONY: test build dev-use dev-stop bump release install-app

test:
	./tests/run.sh

# optional: Rust engine (~10x less CPU); plugin works without it
build:
	cargo build --release

dev-use:
	mise exec rust@latest -- ./scripts/dev-bin.sh use

dev-stop:
	./scripts/dev-bin.sh stop

# macOS: build, sign, and install the Agenmux.app notification helper
install-app:
	./scripts/install-app.sh

# publish the existing local bump commit + tag; never creates or moves either
release:
	./scripts/release.sh

# update RELEASE_NOTES.md, then patch-bump, test, commit, and tag (no push)
bump:
	./scripts/bump.sh
