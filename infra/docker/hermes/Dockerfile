ARG HERMES_VERSION=latest
FROM nousresearch/hermes-agent:${HERMES_VERSION}

# Hermes Hub 只定制运行时默认值；真实模型凭据由 Hub LLM Proxy 注入和代理。
ENV API_SERVER_ENABLED=true
ENV HERMES_HOST=0.0.0.0

WORKDIR /workspace
