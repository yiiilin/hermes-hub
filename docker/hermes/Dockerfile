# 上游 Hermes Agent 不跟随 latest 自动漂移；升级时显式替换这个 tag@digest。
ARG HERMES_AGENT_IMAGE=nousresearch/hermes-agent:v2026.5.29.2@sha256:2bba4ab37729ebdd864d4caf277b24fec4cd8bfc2855185fd9f4c90f9bf7bfa3
FROM ${HERMES_AGENT_IMAGE}

COPY docker/hermes/hermes-hub-entrypoint.sh /opt/hermes-hub/entrypoint.sh
COPY docker/hermes/patch_send_message_tool.py /opt/hermes-hub/patch_send_message_tool.py
COPY docker/hermes/plugins /opt/hermes/plugins
RUN chmod +x /opt/hermes-hub/entrypoint.sh
RUN python3 /opt/hermes-hub/patch_send_message_tool.py

# Hermes Hub 只定制运行时默认值；真实模型凭据由 Hub LLM Proxy 注入和代理。
ENV API_SERVER_ENABLED=true
ENV HERMES_HOST=0.0.0.0

WORKDIR /workspace
ENTRYPOINT ["/opt/hermes-hub/entrypoint.sh"]
