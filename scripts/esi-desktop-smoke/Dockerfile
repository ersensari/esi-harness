FROM debian:bookworm-slim

COPY --from=deb esi-studio_1.48.0_amd64.deb /tmp/esi-studio.deb

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        /tmp/esi-studio.deb \
        dbus-x11 \
        xauth \
        xvfb \
    && rm -rf /var/lib/apt/lists/* /tmp/esi-studio.deb \
    && useradd --create-home --uid 10001 esi-smoke

COPY --chmod=0755 scripts/esi-desktop-smoke/run-smoke.sh /usr/local/bin/run-esi-desktop-smoke

USER esi-smoke
ENV HOME=/home/esi-smoke

ENTRYPOINT ["/usr/local/bin/run-esi-desktop-smoke"]