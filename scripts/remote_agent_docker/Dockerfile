FROM ubuntu:24.04 AS unpack

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends unzip \
    && rm -rf /var/lib/apt/lists/*

COPY package.zip /tmp/package.zip

RUN mkdir -p /opt/xedoc \
    && unzip -q /tmp/package.zip -d /opt/xedoc \
    && chmod 0755 /opt/xedoc/bin/xedoc /opt/xedoc/bin/xedoc-remote-agentd /opt/xedoc/bin/xedoc-session \
    && ln -s bin/xedoc /opt/xedoc/xedoc

FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends bubblewrap procps tini \
    && rm -rf /var/lib/apt/lists/*

COPY --from=unpack /opt/xedoc /opt/xedoc

ENV XEDOC_HOME=/root/.xedoc
ENV PATH=/opt/xedoc/bin:/opt/xedoc/xedoc-path:${PATH}

RUN ln -s /opt/xedoc/xedoc-resources/remote-agent/runtime/python/bin/python3 /usr/local/bin/python3 \
    && mkdir -p "$XEDOC_HOME/remote-agent" "$XEDOC_HOME/packages/standalone/releases" "$XEDOC_HOME/secrets" \
    && touch "$XEDOC_HOME/auth.json" \
    && ln -s /opt/xedoc "$XEDOC_HOME/packages/standalone/current" \
    && printf '%s\n' \
        '[remote_agent]' \
        'role = "coordinator"' \
        'workspaces = { root = "/" }' \
        'managed_coordinator_certificate_paths = []' \
        '' \
        '[remote_agent.limits]' \
        'max_attached_sessions = 8' \
        'max_message_bytes = 16384' \
        'max_result_bytes = 65536' \
        'max_wait_seconds = 120' \
        'max_discovery_seconds = 10' \
        'audit_retention_days = 90' \
        > "$XEDOC_HOME/config.toml" \
    && printf '%s\n' \
        'controller = "unix:///root/.xedoc/app-server-control/app-server-control.sock"' \
        > "$XEDOC_HOME/remote-agent/bootstrap.toml" \
    && chmod 0700 "$XEDOC_HOME/remote-agent" \
    && chmod 0600 "$XEDOC_HOME/auth.json" "$XEDOC_HOME/remote-agent/bootstrap.toml"

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["sleep", "infinity"]
