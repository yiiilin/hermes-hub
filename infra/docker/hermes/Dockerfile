ARG HERMES_VERSION=latest
FROM nousresearch/hermes-agent:${HERMES_VERSION}

COPY infra/docker/hermes/patch_send_message_tool.py /tmp/patch_send_message_tool.py
RUN python3 /tmp/patch_send_message_tool.py && rm /tmp/patch_send_message_tool.py
COPY infra/docker/hermes/hermes-hub-entrypoint.sh /opt/hermes-hub/entrypoint.sh
RUN chmod +x /opt/hermes-hub/entrypoint.sh

# Hermes Hub 只定制运行时默认值；真实模型凭据由 Hub LLM Proxy 注入和代理。
ENV API_SERVER_ENABLED=true
ENV HERMES_HOST=0.0.0.0

WORKDIR /workspace
ENTRYPOINT ["/opt/hermes-hub/entrypoint.sh"]
