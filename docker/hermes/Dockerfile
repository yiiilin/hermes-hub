# 上游 Hermes Agent 不跟随 latest 自动漂移；升级时显式替换这个 tag。
ARG HERMES_AGENT_IMAGE=nousresearch/hermes-agent:v2026.5.29.2
FROM ${HERMES_AGENT_IMAGE}

COPY docker/hermes/hermes-hub-entrypoint.sh /opt/hermes-hub/entrypoint.sh
COPY docker/hermes/plugins /opt/hermes/plugins
RUN chmod +x /opt/hermes-hub/entrypoint.sh

# Hermes Hub 只定制运行时默认值；真实模型凭据由 Hub LLM Proxy 注入和代理。
ENV API_SERVER_ENABLED=true
ENV HERMES_HOST=0.0.0.0

WORKDIR /workspace
ENTRYPOINT ["/opt/hermes-hub/entrypoint.sh"]
