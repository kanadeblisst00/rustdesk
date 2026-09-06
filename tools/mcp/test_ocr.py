import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

import ocr


class OcrProtocolTests(unittest.TestCase):
    def test_invalid_input_returns_structured_error(self):
        result = subprocess.run([sys.executable, str(Path(ocr.__file__))], input=b"not a PNG",
                                capture_output=True, timeout=20, check=True)
        value = json.loads(result.stdout)
        self.assertFalse(value["available"])
        self.assertEqual(value["engine"], "PP-OCRv4")
        self.assertIn("error", value)


@unittest.skipUnless(importlib.util.find_spec("rapidocr_onnxruntime"), "install OCR requirements for inference tests")
class OcrInferenceTests(unittest.TestCase):
    def test_real_ppocrv4_inference_and_original_coordinates(self):
        import cv2
        import numpy as np
        image = np.full((240, 800, 3), 255, np.uint8)
        cv2.putText(image, "SAVE SETTINGS", (100, 130), cv2.FONT_HERSHEY_SIMPLEX, 1.5, (0, 0, 0), 3)
        ok, encoded = cv2.imencode(".png", image)
        self.assertTrue(ok)
        result = ocr.recognize(encoded.tobytes())
        self.assertEqual((result["width"], result["height"]), (800, 240))
        self.assertIn("SAVE", " ".join(b["text"] for b in result["blocks"]).upper())
        target = next(b for b in result["blocks"] if "SAVE" in b["text"].upper())
        self.assertGreater(target["confidence"], 0.8)
        self.assertLess(abs(target["bounds"]["x"] - 100), 20)
        self.assertGreater(target["bounds"]["width"], 200)

    def test_clips_boxes_and_filters_nonfinite_low_confidence_results(self):
        import cv2
        import numpy as np
        _, image = cv2.imencode(".png", np.full((80, 200, 3), 255, np.uint8))
        def fake(_):
            return [
                ([[-4, 2], [210, 2], [210, 90], [-4, 90]], "保存", 0.99),
                ([[0, 0]] * 4, "low", 0.1),
                ([[float("nan"), 0]] * 4, "bad", 0.9),
            ], None
        result = ocr.recognize(image.tobytes(), engine=fake)
        self.assertEqual(len(result["blocks"]), 1)
        self.assertEqual(result["blocks"][0]["bounds"], {"x": 0, "y": 2, "width": 200, "height": 78})


if __name__ == "__main__":
    unittest.main()
