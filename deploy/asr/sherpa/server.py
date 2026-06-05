import asyncio
import hashlib
import json
import os
import shutil
import tarfile
import urllib.request
from contextlib import asynccontextmanager
from pathlib import Path
from threading import Lock

import numpy as np
import sherpa_onnx
import uvicorn
from fastapi import FastAPI, WebSocket, WebSocketDisconnect


HOST = os.getenv("HOST", "0.0.0.0")
PORT = int(os.getenv("PORT", "9991"))
MODEL_ROOT = Path(os.getenv("MODEL_ROOT", "/models"))
MODEL_ARCHIVE = os.getenv(
    "MODEL_ARCHIVE", "sherpa-onnx-streaming-paraformer-bilingual-zh-en.tar.bz2"
)
MODEL_DIR = os.getenv("MODEL_DIR", "sherpa-onnx-streaming-paraformer-bilingual-zh-en")
ENCODER_FILE = os.getenv("MODEL_FILE", "encoder.int8.onnx")
DECODER_FILE = os.getenv("DECODER_FILE", "decoder.int8.onnx")
TOKENS_FILE = os.getenv("TOKENS_FILE", "tokens.txt")
NUM_THREADS = int(os.getenv("NUM_THREADS", "4"))
MAX_AUDIO_SECONDS = int(os.getenv("MAX_AUDIO_SECONDS", "60"))
DOWNLOAD_TIMEOUT_SECONDS = int(os.getenv("DOWNLOAD_TIMEOUT_SECONDS", "60"))
DOWNLOAD_RETRIES = int(os.getenv("DOWNLOAD_RETRIES", "3"))
MODEL_SHA256 = os.getenv("MODEL_SHA256", "").strip().lower()
MODEL_URL = os.getenv(
    "MODEL_URL",
    f"https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/{MODEL_ARCHIVE}",
)
SAMPLE_RATE = 16000
FEATURE_DIM = 80
TAIL_PADDING_SECONDS = 0.66
DOWNLOAD_CHUNK_SIZE = 1024 * 1024
MAX_PCM_BYTES = SAMPLE_RATE * MAX_AUDIO_SECONDS * 2 if MAX_AUDIO_SECONDS > 0 else 0

recognizer = None
recognizer_lock = Lock()


class AudioTooLongError(ValueError):
    pass


def archive_member_is_safe(member_name: str, target: Path) -> bool:
    """只允许模型压缩包解压到模型目录内部，避免异常 tar 路径越界。"""
    destination = (target / member_name).resolve()
    return destination == target.resolve() or destination.is_relative_to(target.resolve())


def normalize_tar_member(member: tarfile.TarInfo, target: Path) -> tarfile.TarInfo:
    """只接受普通文件和目录，拒绝符号链接/硬链接/设备文件等高风险 member。"""
    if not archive_member_is_safe(member.name, target):
        raise RuntimeError(f"unsafe model archive member: {member.name}")
    if not (member.isfile() or member.isdir()):
        raise RuntimeError(f"unsupported model archive member type: {member.name}")
    member.uid = 0
    member.gid = 0
    member.uname = ""
    member.gname = ""
    member.mode = 0o755 if member.isdir() else 0o644
    return member


def extract_model_archive(archive_path: Path, target: Path) -> None:
    with tarfile.open(archive_path, "r:bz2") as archive:
        members = [normalize_tar_member(member, target) for member in archive.getmembers()]
        archive.extractall(target, members=members)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(DOWNLOAD_CHUNK_SIZE), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download_model_archive(archive_path: Path) -> None:
    """下载到 .part 后再原子替换，避免半下载文件被后续启动误用。"""
    part_path = archive_path.with_name(f"{archive_path.name}.part")
    last_error: Exception | None = None
    for attempt in range(1, DOWNLOAD_RETRIES + 1):
        part_path.unlink(missing_ok=True)
        try:
            print(f"Downloading model from {MODEL_URL} (attempt {attempt})", flush=True)
            with urllib.request.urlopen(MODEL_URL, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:
                with part_path.open("wb") as output:
                    shutil.copyfileobj(response, output, length=DOWNLOAD_CHUNK_SIZE)
            if MODEL_SHA256:
                actual_sha256 = sha256_file(part_path)
                if actual_sha256 != MODEL_SHA256:
                    raise RuntimeError(
                        f"model archive sha256 mismatch: expected {MODEL_SHA256}, got {actual_sha256}"
                    )
            part_path.replace(archive_path)
            return
        except Exception as exc:  # noqa: BLE001 - 启动阶段需要记录并重试下载/校验失败。
            last_error = exc
            part_path.unlink(missing_ok=True)
            print(f"Model download failed: {exc}", flush=True)
    raise RuntimeError(f"model download failed after {DOWNLOAD_RETRIES} attempts: {last_error}")


def validated_model_paths(model_dir: Path) -> tuple[Path, Path, Path]:
    encoder_path = model_dir / ENCODER_FILE
    decoder_path = model_dir / DECODER_FILE
    tokens_path = model_dir / TOKENS_FILE
    if not encoder_path.is_file() or not decoder_path.is_file() or not tokens_path.is_file():
        raise RuntimeError(f"streaming model files not found under {model_dir}")
    return encoder_path, decoder_path, tokens_path


def ensure_model() -> tuple[Path, Path, Path]:
    """确保本地流式模型存在；不存在时下载官方 sherpa-onnx streaming Paraformer 模型。"""
    MODEL_ROOT.mkdir(parents=True, exist_ok=True)
    model_dir = MODEL_ROOT / MODEL_DIR
    try:
        return validated_model_paths(model_dir)
    except RuntimeError:
        pass

    archive_path = MODEL_ROOT / MODEL_ARCHIVE
    staging_dir = MODEL_ROOT / f".{MODEL_DIR}.staging"
    shutil.rmtree(staging_dir, ignore_errors=True)
    staging_dir.mkdir(parents=True, exist_ok=True)
    try:
        download_model_archive(archive_path)
        print(f"Extracting {archive_path}", flush=True)
        extract_model_archive(archive_path, staging_dir)
        extracted_model_dir = staging_dir / MODEL_DIR
        validated_model_paths(extracted_model_dir)
        shutil.rmtree(model_dir, ignore_errors=True)
        extracted_model_dir.replace(model_dir)
        return validated_model_paths(model_dir)
    finally:
        archive_path.unlink(missing_ok=True)
        shutil.rmtree(staging_dir, ignore_errors=True)


@asynccontextmanager
async def lifespan(app: FastAPI):
    """启动时加载模型，避免用户第一次按住说话时才付出加载成本。"""
    del app
    global recognizer
    encoder_path, decoder_path, tokens_path = ensure_model()
    try:
        recognizer = load_recognizer(encoder_path, decoder_path, tokens_path)
    except Exception:
        # 持久化目录里如果残留了旧的坏模型，清理后重新走下载/校验/原子替换流程。
        shutil.rmtree(MODEL_ROOT / MODEL_DIR, ignore_errors=True)
        encoder_path, decoder_path, tokens_path = ensure_model()
        recognizer = load_recognizer(encoder_path, decoder_path, tokens_path)
    print(
        json.dumps(
            {
                "status": "UP",
                "engine": "sherpa-onnx",
                "model": str(MODEL_ROOT / MODEL_DIR),
                "encoder": str(encoder_path),
                "decoder": str(decoder_path),
                "tokens": str(tokens_path),
                "num_threads": NUM_THREADS,
                "sample_rate": SAMPLE_RATE,
                "max_audio_seconds": MAX_AUDIO_SECONDS,
            },
            ensure_ascii=False,
        ),
        flush=True,
    )
    yield
    recognizer = None


app = FastAPI(title="Hermes Hub sherpa-onnx streaming ASR", lifespan=lifespan)


def load_recognizer(encoder_path: Path, decoder_path: Path, tokens_path: Path):
    return sherpa_onnx.OnlineRecognizer.from_paraformer(
        tokens=str(tokens_path),
        encoder=str(encoder_path),
        decoder=str(decoder_path),
        num_threads=NUM_THREADS,
        sample_rate=SAMPLE_RATE,
        feature_dim=FEATURE_DIM,
        decoding_method="greedy_search",
        debug=False,
    )


@app.get("/")
def root() -> dict:
    return {
        "message": "Hermes Hub sherpa-onnx streaming ASR",
        "endpoints": {
            "health": "/health",
            "stream": "/stream",
        },
    }


@app.get("/health")
def health() -> dict:
    return {
        "status": "UP" if recognizer is not None else "DOWN",
        "engine": "sherpa-onnx",
        "model": MODEL_DIR,
        "sample_rate": SAMPLE_RATE,
        "num_threads": NUM_THREADS,
    }


def pcm16le_to_float32(payload: bytes) -> np.ndarray:
    """浏览器侧已经重采样为 16k 单声道 PCM16 LE；这里只做轻量归一化。"""
    if len(payload) % 2 != 0:
        payload = payload[:-1]
    if not payload:
        return np.array([], dtype=np.float32)
    pcm = np.frombuffer(payload, dtype="<i2").astype(np.float32)
    return np.ascontiguousarray(pcm / 32768.0)


def decode_ready_chunks(stream) -> str:
    if recognizer is None:
        raise RuntimeError("recognizer is not loaded")
    with recognizer_lock:
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
        result = recognizer.get_result(stream)
    return str(getattr(result, "text", result)).strip()


def decode_final(stream) -> str:
    if recognizer is None:
        raise RuntimeError("recognizer is not loaded")
    tail_padding = np.zeros(int(SAMPLE_RATE * TAIL_PADDING_SECONDS), dtype=np.float32)
    stream.accept_waveform(SAMPLE_RATE, tail_padding)
    input_finished = getattr(stream, "input_finished", None)
    if callable(input_finished):
        input_finished()
    with recognizer_lock:
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)
        result = recognizer.get_result(stream)
    return str(getattr(result, "text", result)).strip()


async def send_stream_error(websocket: WebSocket, message: str) -> None:
    await websocket.send_json({"type": "error", "message": message})


@app.websocket("/stream")
async def stream_asr(websocket: WebSocket):
    await websocket.accept()
    if recognizer is None:
        await send_stream_error(websocket, "recognizer is not loaded")
        await websocket.close()
        return

    stream = recognizer.create_stream()
    total_pcm_bytes = 0
    started = False
    try:
        while True:
            message = await websocket.receive()
            if message.get("type") == "websocket.disconnect":
                break

            if "text" in message:
                try:
                    event = json.loads(message["text"])
                except json.JSONDecodeError:
                    await send_stream_error(websocket, "stream event is invalid json")
                    continue
                event_type = event.get("type")
                if event_type == "start":
                    sample_rate = int(event.get("sample_rate") or SAMPLE_RATE)
                    if sample_rate != SAMPLE_RATE:
                        await send_stream_error(websocket, "sample_rate must be 16000")
                        await websocket.close()
                        return
                    started = True
                    continue
                if event_type == "stop":
                    text = await asyncio.to_thread(decode_final, stream)
                    await websocket.send_json({"type": "final", "text": text})
                    await websocket.send_json({"type": "done"})
                    await websocket.close()
                    return
                await send_stream_error(websocket, "stream event type is unsupported")
                continue

            payload = message.get("bytes")
            if payload is None:
                continue
            if not started:
                await send_stream_error(websocket, "stream must start before audio")
                continue
            total_pcm_bytes += len(payload)
            if MAX_PCM_BYTES > 0 and total_pcm_bytes > MAX_PCM_BYTES:
                raise AudioTooLongError("audio duration is too long")
            samples = pcm16le_to_float32(payload)
            if samples.size == 0:
                continue
            stream.accept_waveform(SAMPLE_RATE, samples)
            text = await asyncio.to_thread(decode_ready_chunks, stream)
            if text:
                await websocket.send_json({"type": "partial", "text": text})
    except WebSocketDisconnect:
        return
    except AudioTooLongError:
        await send_stream_error(websocket, "audio duration is too long")
        await websocket.close()
    except Exception as exc:  # noqa: BLE001 - ASR 服务边界需要把内部失败降级为协议错误。
        print(f"Streaming ASR failed: {exc}", flush=True)
        await send_stream_error(websocket, "asr failed")
        await websocket.close()


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT)
