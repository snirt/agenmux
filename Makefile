CONTAINER_ENGINE ?= $(shell command -v docker 2>/dev/null || command -v podman 2>/dev/null)
CONTAINER_IMAGE ?= agenmux-dev
CONTAINER_TMUX_CONF_ARGS = $(if $(AGENMUX_CONTAINER_TMUX_CONF),--volume "$(abspath $(AGENMUX_CONTAINER_TMUX_CONF)):/tmp/agenmux-tmux.conf:ro" --env AGENMUX_CONTAINER_TMUX_CONF=/tmp/agenmux-tmux.conf)

.PHONY: test build clean dev-use dev-stop bump release install-app container-build container-test container-use container-install container-install-local check-container-engine
test:
	./tests/run.sh

check-container-engine:
	@test -n "$(CONTAINER_ENGINE)" || { echo "Docker or Podman is required" >&2; exit 1; }

container-build: check-container-engine
	$(CONTAINER_ENGINE) build --file Containerfile --tag "$(CONTAINER_IMAGE)" .

container-test: container-build
	$(CONTAINER_ENGINE) run --rm --init "$(CONTAINER_IMAGE)" test

container-use: container-build
	$(CONTAINER_ENGINE) run --rm --init -it $(CONTAINER_TMUX_CONF_ARGS) \
		--volume "$(CURDIR):/workspace" \
		--env CARGO_TARGET_DIR=/tmp/agenmux-target \
		"$(CONTAINER_IMAGE)" use

container-install: container-build
	$(CONTAINER_ENGINE) run --rm --init -it $(CONTAINER_TMUX_CONF_ARGS) \
		"$(CONTAINER_IMAGE)" install

container-install-local: container-build
	$(CONTAINER_ENGINE) run --rm --init -it $(CONTAINER_TMUX_CONF_ARGS) \
		"$(CONTAINER_IMAGE)" install-local

# optional: Rust engine (~10x less CPU); plugin works without it
build:
	cargo build --release

clean:
	cargo clean

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
