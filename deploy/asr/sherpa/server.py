import asyncio
import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
from contextlib import asynccontextmanager
from pathlib import Path
from threading import Lock

import numpy as np
import sherpa_onnx
import soundfile as sf
import uvicorn
from fastapi import FastAPI, File, Form, UploadFile
from fastapi.responses import JSONResponse


HOST = os.getenv("HOST", "0.0.0.0")
PORT = int(os.getenv("PORT", "9991"))
MODEL_ROOT = Path(os.getenv("MODEL_ROOT", "/models"))
MODEL_ARCHIVE = os.getenv(
    "MODEL_ARCHIVE", "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2"
)
MODEL_DIR = os.getenv("MODEL_DIR", "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17")
MODEL_FILE = os.getenv("MODEL_FILE", "model.int8.onnx")
TOKENS_FILE = os.getenv("TOKENS_FILE", "tokens.txt")
USE_ITN = os.getenv("USE_ITN", "true").lower() in {"1", "true", "yes", "on"}
NUM_THREADS = int(os.getenv("NUM_THREADS", "4"))
MAX_FILE_SIZE = int(os.getenv("MAX_FILE_SIZE", "26214400"))
MAX_AUDIO_SECONDS = int(os.getenv("MAX_AUDIO_SECONDS", "60"))
DOWNLOAD_TIMEOUT_SECONDS = int(os.getenv("DOWNLOAD_TIMEOUT_SECONDS", "60"))
DOWNLOAD_RETRIES = int(os.getenv("DOWNLOAD_RETRIES", "3"))
FFMPEG_TIMEOUT_SECONDS = int(os.getenv("FFMPEG_TIMEOUT_SECONDS", str(max(30, MAX_AUDIO_SECONDS * 3))))
MODEL_SHA256 = os.getenv("MODEL_SHA256", "").strip().lower()
MODEL_URL = os.getenv(
    "MODEL_URL",
    f"https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/{MODEL_ARCHIVE}",
)
UPLOAD_CHUNK_SIZE = 1024 * 1024

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
        for chunk in iter(lambda: source.read(UPLOAD_CHUNK_SIZE), b""):
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
                    shutil.copyfileobj(response, output, length=UPLOAD_CHUNK_SIZE)
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


def validated_model_paths(model_dir: Path) -> tuple[Path, Path]:
    model_path = model_dir / MODEL_FILE
    tokens_path = model_dir / TOKENS_FILE
    if not model_path.is_file() or not tokens_path.is_file():
        raise RuntimeError(f"model files not found under {model_dir}")
    return model_path, tokens_path


def ensure_model() -> tuple[Path, Path]:
    """确保本地模型存在；不存在时下载官方 sherpa-onnx SenseVoice int8 模型。"""
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
    model_path, tokens_path = ensure_model()
    try:
        recognizer = load_recognizer(model_path, tokens_path)
    except Exception:
        # 持久化目录里如果残留了旧的坏模型，清理后重新走下载/校验/原子替换流程。
        shutil.rmtree(MODEL_ROOT / MODEL_DIR, ignore_errors=True)
        model_path, tokens_path = ensure_model()
        recognizer = load_recognizer(model_path, tokens_path)
    print(
        json.dumps(
            {
                "status": "UP",
                "engine": "sherpa-onnx",
                "model": str(model_path),
                "tokens": str(tokens_path),
                "use_itn": USE_ITN,
                "num_threads": NUM_THREADS,
                "max_audio_seconds": MAX_AUDIO_SECONDS,
                "max_file_size": MAX_FILE_SIZE,
            },
            ensure_ascii=False,
        ),
        flush=True,
    )
    yield
    recognizer = None


app = FastAPI(title="Hermes Hub sherpa-onnx ASR", lifespan=lifespan)


def load_recognizer(model_path: Path, tokens_path: Path):
    return sherpa_onnx.OfflineRecognizer.from_sense_voice(
        model=str(model_path),
        tokens=str(tokens_path),
        num_threads=NUM_THREADS,
        use_itn=USE_ITN,
        debug=False,
    )


@app.get("/")
def root() -> dict:
    return {
        "message": "Hermes Hub sherpa-onnx ASR",
        "endpoints": {
            "health": "/health",
            "audio_transcriptions": "/v1/audio/transcriptions",
        },
    }


@app.get("/health")
def health() -> dict:
    return {
        "status": "UP" if recognizer is not None else "DOWN",
        "engine": "sherpa-onnx",
        "model": MODEL_FILE,
        "use_itn": USE_ITN,
        "num_threads": NUM_THREADS,
    }


def convert_to_wav(input_path: Path, output_path: Path) -> None:
    """统一把浏览器上传的 webm/ogg/wav 转成单声道 16k wav。"""
    subprocess.run(
        [
            "ffmpeg",
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            str(input_path),
            "-ac",
            "1",
            "-ar",
            "16000",
            "-f",
            "wav",
            str(output_path),
        ],
        check=True,
        timeout=FFMPEG_TIMEOUT_SECONDS,
    )


def decode_audio(audio_path: Path) -> str:
    if recognizer is None:
        raise RuntimeError("recognizer is not loaded")
    samples, sample_rate = sf.read(audio_path, dtype="float32", always_2d=True)
    audio = np.ascontiguousarray(samples[:, 0])
    if MAX_AUDIO_SECONDS > 0 and len(audio) / sample_rate > MAX_AUDIO_SECONDS:
        raise AudioTooLongError("audio duration is too long")
    stream = recognizer.create_stream()
    stream.accept_waveform(sample_rate, audio)
    with recognizer_lock:
        recognizer.decode_stream(stream)
    result = stream.result
    text = getattr(result, "text", None)
    if text is not None:
        return text
    try:
        parsed = json.loads(str(result))
        return str(parsed.get("text", "")).strip()
    except json.JSONDecodeError:
        return str(result).strip()


async def spool_upload_to_path(file: UploadFile, input_path: Path) -> int:
    """分块保存上传音频，超过上限立即中止，避免把大请求完整读入内存。"""
    size = 0
    with input_path.open("wb") as output:
        while True:
            chunk = await file.read(UPLOAD_CHUNK_SIZE)
            if not chunk:
                break
            size += len(chunk)
            if size > MAX_FILE_SIZE:
                raise ValueError("audio file is too large")
            output.write(chunk)
    return size


def openai_error(status_code: int, message: str, code: str, error_type: str = "invalid_request_error"):
    return JSONResponse(
        status_code=status_code,
        content={
            "error": {
                "message": message,
                "type": error_type,
                "code": code,
            }
        },
    )


@app.post("/v1/audio/transcriptions")
async def transcribe(
    file: UploadFile = File(...),
    model: str | None = Form(None),
    language: str | None = Form(None),
    response_format: str | None = Form(None),
) -> dict:
    """OpenAI 兼容的短音频转写接口；模型选择由容器环境变量固定。"""
    del model, language
    if response_format not in (None, "", "json"):
        return openai_error(400, "response_format only supports json", "unsupported_response_format")
    suffix = Path(file.filename or "audio.webm").suffix or ".webm"

    with tempfile.TemporaryDirectory() as temp_dir:
        input_path = Path(temp_dir) / f"input{suffix}"
        wav_path = Path(temp_dir) / "audio.wav"
        try:
            await spool_upload_to_path(file, input_path)
            convert_to_wav(input_path, wav_path)
            text = await asyncio.to_thread(decode_audio, wav_path)
        except ValueError as exc:
            if str(exc) == "audio file is too large":
                return openai_error(413, "audio file is too large", "audio_file_too_large")
            raise
        except AudioTooLongError:
            return openai_error(413, "audio duration is too long", "audio_duration_too_long")
        except subprocess.CalledProcessError as exc:
            return openai_error(400, "audio conversion failed", "audio_conversion_failed")
        except subprocess.TimeoutExpired:
            return openai_error(408, "audio conversion timed out", "audio_conversion_timeout")
        except Exception as exc:
            print(f"ASR failed: {exc}", flush=True)
            return openai_error(500, "asr failed", "asr_failed", "server_error")
    return {"text": text}


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT)
