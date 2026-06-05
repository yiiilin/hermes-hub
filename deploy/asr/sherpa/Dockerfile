FROM python:3.11-slim@sha256:a3ab0b966bc4e91546a033e22093cb840908979487a9fc0e6e38295747e49ac0

ARG DEBIAN_SNAPSHOT=20260518T000000Z

# Hermes Hub ASR 镜像：用 sherpa-onnx 原生流式 API 做实时识别，只维护一层很薄的 WS 包装。
RUN sed -i "s|URIs: http://deb.debian.org/debian-security|URIs: http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}|g" /etc/apt/sources.list.d/debian.sources \
    && sed -i "s|URIs: http://deb.debian.org/debian|URIs: http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}|g" /etc/apt/sources.list.d/debian.sources \
    && printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99snapshot \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        bzip2=1.0.8-6 \
        ca-certificates=20250419 \
        curl=8.14.1-2+deb13u3 \
    && rm -rf /var/lib/apt/lists/*

COPY requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir -r /tmp/requirements.txt

WORKDIR /app
COPY server.py /app/server.py

ENV HOST=0.0.0.0 \
    PORT=9991 \
    MODEL_ROOT=/models \
    MODEL_ARCHIVE=sherpa-onnx-streaming-paraformer-bilingual-zh-en.tar.bz2 \
    MODEL_DIR=sherpa-onnx-streaming-paraformer-bilingual-zh-en \
    MODEL_FILE=encoder.int8.onnx \
    DECODER_FILE=decoder.int8.onnx \
    TOKENS_FILE=tokens.txt \
    MODEL_SHA256=5462a1fce42693deae572af1e8c4687124b12aa85fe61ff4d3168bb5280e205f \
    NUM_THREADS=4 \
    MAX_AUDIO_SECONDS=60 \
    DOWNLOAD_TIMEOUT_SECONDS=60 \
    DOWNLOAD_RETRIES=3

CMD ["python", "/app/server.py"]
