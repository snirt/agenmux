.PHONY: test build bump install-app

test:
	./tests/run.sh

# optional: Rust engine (~10x less CPU); plugin works without it
build:
	cargo build --release

# macOS: build, sign, and install the AgentsMon.app notification helper
install-app:
	./scripts/install-app.sh

# patch-bump Cargo.toml, run both suites, commit + tag (no push)
bump:
	@old=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml) && \
	new=$${old%.*}.$$(( $${old##*.} + 1 )) && \
	sed -i '' "s/^version = \"$$old\"/version = \"$$new\"/" Cargo.toml && \
	cargo test && ./tests/run.sh && \
	git add Cargo.toml Cargo.lock && \
	git commit -m "chore: bump version to $$new" && \
	git tag "v$$new" && \
	echo "bumped $$old -> $$new, tagged v$$new (not pushed)"
