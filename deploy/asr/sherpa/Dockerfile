FROM python:3.11-slim@sha256:a3ab0b966bc4e91546a033e22093cb840908979487a9fc0e6e38295747e49ac0

ARG DEBIAN_SNAPSHOT=20260518T000000Z

# Hermes Hub ASR 镜像：用成熟的 sherpa-onnx 做推理，只维护一层很薄的 OpenAI 兼容 HTTP 包装。
RUN sed -i "s|URIs: http://deb.debian.org/debian-security|URIs: http://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}|g" /etc/apt/sources.list.d/debian.sources \
    && sed -i "s|URIs: http://deb.debian.org/debian|URIs: http://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}|g" /etc/apt/sources.list.d/debian.sources \
    && printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99snapshot \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        bzip2=1.0.8-6 \
        ca-certificates=20250419 \
        curl=8.14.1-2+deb13u3 \
        ffmpeg=7:7.1.4-0+deb13u1 \
    && rm -rf /var/lib/apt/lists/*

COPY requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir -r /tmp/requirements.txt

WORKDIR /app
COPY server.py /app/server.py

ENV HOST=0.0.0.0 \
    PORT=9991 \
    MODEL_ROOT=/models \
    MODEL_ARCHIVE=sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2 \
    MODEL_DIR=sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17 \
    MODEL_FILE=model.int8.onnx \
    TOKENS_FILE=tokens.txt \
    MODEL_SHA256=7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e \
    USE_ITN=true \
    NUM_THREADS=4 \
    MAX_FILE_SIZE=26214400 \
    MAX_AUDIO_SECONDS=60 \
    DOWNLOAD_TIMEOUT_SECONDS=60 \
    DOWNLOAD_RETRIES=3 \
    FFMPEG_TIMEOUT_SECONDS=180

CMD ["python", "/app/server.py"]
