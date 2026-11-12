# Resultados del banco de pruebas

Kodak (24 imágenes, 768×512). WebP: libwebp 1.6.0 `method=6`. AVIF: libavif 1.4.2 (aom) `speed=4`, 4:4:4. JPEG XL: libjxl 0.12 (imagecodecs), `effort=7`. Tesela: perfil por defecto y `--tune psnr`. Métricas calculadas con `tesela compare` para todos los códecs. Las probabilidades iniciales de Tesela se entrenaron con BSD100 e imágenes sintéticas propias, nunca con las de evaluación.

Imágenes: 24. BD-rate frente a **WebP** (negativo = menos bytes a igual calidad).

| Códec | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Cod. (s/img) | Dec. (s/img) |
|---|---|---|---|---|---|---|---|
| TESELA | -30.4 % | -26.1 % | -16.8 % | -25.5 % | -27.1 % | 2.71 | 0.092 |
| TESELA-PSNR | -15.7 % | -13.2 % | -28.0 % | -23.3 % | -30.3 % | 3.04 | 0.088 |
| WEBP | +0.0 % | +0.0 % | +0.0 % | +0.0 % | +0.0 % | 0.13 | 0.115 |
| AVIF | -26.3 % | -22.4 % | -12.9 % | -19.3 % | -20.5 % | 2.50 | 0.256 |
| JXL | -12.1 % | -13.4 % | +9.4 % | +4.4 % | +12.2 % | 0.19 | 0.146 |

Rangos de calidad sobre los que se integra el BD-rate: ssimulacra2: 30–90, ssimulacra2_hq: 70–90, psnr_y: 28–48, ssim_y: 0.85–0.995, psnr: 26–46.

![curvas](curves.png)
