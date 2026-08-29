FROM rust:1.96.1-bookworm

WORKDIR /workspace
COPY . .

RUN cargo test --locked -p esi-development-visualizer --test team_install_smoke --no-run

ENV ESI_CARGO=/usr/local/cargo/bin/cargo \
    CARGO_NET_OFFLINE=true \
    ESI_TEAM_SMOKE_NETWORK_ISOLATED=1

CMD ["sh", "scripts/esi-team-smoke/run-smoke.sh"]