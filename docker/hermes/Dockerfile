ARG HERMES_VERSION=latest
FROM nousresearch/hermes-agent:${HERMES_VERSION}

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
