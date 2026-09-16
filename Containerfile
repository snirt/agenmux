FROM rust:1.90-bookworm AS base

ENV LANG=C.UTF-8 LC_ALL=C.UTF-8

ARG TMUX_VERSION=3.7b
ARG TMUX_SHA256=87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        bison \
        ca-certificates \
        curl \
        expect \
        git \
        libevent-dev \
        libncurses-dev \
        pkg-config \
        procps \
    && rm -rf /var/lib/apt/lists/*

RUN curl --fail --location --silent --show-error \
        "https://github.com/tmux/tmux/releases/download/${TMUX_VERSION}/tmux-${TMUX_VERSION}.tar.gz" \
        --output /tmp/tmux.tar.gz \
    && echo "${TMUX_SHA256}  /tmp/tmux.tar.gz" | sha256sum --check --strict \
    && mkdir /tmp/tmux \
    && tar -xzf /tmp/tmux.tar.gz -C /tmp/tmux --strip-components=1 \
    && cd /tmp/tmux \
    && ./configure \
    && make -j"$(nproc)" \
    && make install \
    && rm -rf /tmp/tmux /tmp/tmux.tar.gz

WORKDIR /workspace

FROM base AS test
COPY . .
RUN install -m 755 scripts/container-entrypoint.sh /usr/local/bin/agenmux-container \
    && CARGO_TARGET_DIR=/tmp/agenmux-target cargo build --release --locked

ENTRYPOINT ["/usr/local/bin/agenmux-container"]
CMD ["test"]
