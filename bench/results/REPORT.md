# Resultados del banco de pruebas

Kodak (24 imágenes, 768×512). WebP: libwebp 1.6.0 `method=6`. AVIF: libavif 1.4.2 (aom) `speed=4`, 4:2:0. JPEG XL: libjxl 0.12 (imagecodecs), `effort=7`. Tesela: perfil por defecto y `--tune psnr`. Métricas calculadas con `tesela compare` para todos los códecs. Las probabilidades iniciales de Tesela se entrenaron con BSD100 e imágenes sintéticas propias, nunca con las de evaluación.

Imágenes: 24. BD-rate frente a **WebP** (negativo = menos bytes a igual calidad).

| Códec | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Cod. (s/img) | Dec. (s/img) |
|---|---|---|---|---|---|---|---|
| TESELA | -26.6 % | -23.4 % | -17.7 % | -24.2 % | -22.8 % | 1.06 | 0.046 |
| TESELA-PSNR | -7.6 % | -7.6 % | -25.8 % | -20.3 % | -25.6 % | 1.07 | 0.042 |
| WEBP | +0.0 % | +0.0 % | +0.0 % | +0.0 % | +0.0 % | 0.13 | 0.115 |
| AVIF | -24.8 % | -19.7 % | -16.4 % | -22.1 % | -17.4 % | 1.42 | 0.164 |
| JXL | -12.1 % | -13.4 % | +9.4 % | +4.4 % | +12.2 % | 0.19 | 0.146 |

Rangos de calidad sobre los que se integra el BD-rate: ssimulacra2: 30–90, ssimulacra2_hq: 70–90, psnr_y: 28–48, ssim_y: 0.85–0.995, psnr: 26–46.

![curvas](curves.png)
