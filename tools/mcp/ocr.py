"""Embedded PP-OCRv4 helper. stdin: lossless PNG bytes; stdout: one JSON object."""

import contextlib
import importlib.metadata
import json
import math
from pathlib import Path
import sys


def recognize(data, engine=None):
    import cv2
    import numpy as np

    image = cv2.imdecode(np.frombuffer(data, dtype=np.uint8), cv2.IMREAD_COLOR)
    if image is None or image.size > 64 * 1024 * 1024:
        raise ValueError("Invalid or oversized remote PNG")
    height, width = image.shape[:2]
    if engine is None:
        if importlib.metadata.version("rapidocr-onnxruntime") != "1.4.4":
            raise RuntimeError("Install tools/mcp/ocr-requirements.txt (RapidOCR 1.4.4 required)")
        import rapidocr_onnxruntime
        from rapidocr_onnxruntime import RapidOCR

        models = Path(rapidocr_onnxruntime.__file__).parent / "models"
        engine = RapidOCR(
            det_model_path=str(models / "ch_PP-OCRv4_det_infer.onnx"),
            rec_model_path=str(models / "ch_PP-OCRv4_rec_infer.onnx"),
            intra_op_num_threads=2, inter_op_num_threads=1,
        )
    result, _ = engine(image)
    blocks = []
    for points, text, score in result or []:
        if not text or not math.isfinite(float(score)) or float(score) < 0.5:
            continue
        if any(not math.isfinite(float(v)) for point in points for v in point):
            continue
        xs, ys = [float(p[0]) for p in points], [float(p[1]) for p in points]
        left, top = max(0, math.floor(min(xs))), max(0, math.floor(min(ys)))
        right, bottom = min(width, math.ceil(max(xs))), min(height, math.ceil(max(ys)))
        if right <= left or bottom <= top:
            continue
        blocks.append({"source": "ocr", "text": text[:4096], "confidence": float(score),
                       "bounds": {"x": left, "y": top, "width": right-left, "height": bottom-top}})
    blocks.sort(key=lambda b: (b["bounds"]["y"], b["bounds"]["x"]))
    return {"available": True, "engine": "PP-OCRv4", "blocks": blocks[:512],
            "truncated": len(blocks) > 512, "width": width, "height": height}


def main():
    try:
        data = sys.stdin.buffer.read(64 * 1024 * 1024 + 1)
        if len(data) > 64 * 1024 * 1024:
            raise ValueError("Remote PNG exceeds 64 MiB")
        with contextlib.redirect_stdout(sys.stderr):
            result = recognize(data)
    except Exception as error:
        result = {"available": False, "engine": "PP-OCRv4", "error": str(error)}
    sys.stdout.write(json.dumps(result, ensure_ascii=True, allow_nan=False))


if __name__ == "__main__":
    main()
