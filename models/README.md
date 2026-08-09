# AI models

The application needs two ONNX models, which are **not** committed to the
repository because of their size. Fetch them with:

```bash
# Windows
powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1

# macOS / Linux
bash scripts/fetch-models.sh
```

| File | Role | Family |
| --- | --- | --- |
| `det_10g.onnx` | Face detection + 5-point landmarks | SCRFD-10G (InsightFace buffalo_l) |
| `w600k_r50.onnx` | 512-d face embedding | ArcFace ResNet-50 (InsightFace buffalo_l) |

## Where models are loaded from

At runtime the application reads models from its **app data** folder:

- Windows: `%APPDATA%\com.teorganiser.desktop\models`
- macOS: `~/Library/Application Support/com.teorganiser.desktop/models`

The fetch scripts copy models there automatically when the folder exists (i.e.
after the app has been launched once).

## Swapping models

The pipeline is not hard-coded to these files (`crates/face-detection`,
`crates/face-recognition` expose trait-based interfaces). Any SCRFD-style
detector export or any 112×112 ArcFace-compatible embedding model can be
dropped into the models folder; files are classified by name — see
`apps/desktop/src-tauri/src/models.rs` — and selectable in Settings.
