# Resultados del banco de pruebas

Kodak (24 imágenes, 768×512). WebP: libwebp 1.6.0 `method=6`. AVIF: libavif 1.4.2 (aom) `speed=4`, 4:2:0. JPEG XL: libjxl 0.12 (imagecodecs), `effort=7`. Tesela: perfil por defecto (equilibrado) y `--tune psnr`. Métricas calculadas con `tesela compare` para todos los códecs. Las probabilidades iniciales de Tesela se entrenaron con BSD100, no con Kodak.

Imágenes: 24. BD-rate frente a **WebP** (negativo = menos bytes a igual calidad).

| Códec | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Cod. (s/img) | Dec. (s/img) |
|---|---|---|---|---|---|---|---|
| TESELA | -26.7 % | -23.4 % | -17.8 % | -24.3 % | -22.8 % | 1.78 | 0.062 |
| TESELA-PSNR | -7.4 % | -7.4 % | -25.8 % | -20.4 % | -25.7 % | 2.02 | 0.060 |
| WEBP | +0.0 % | +0.0 % | +0.0 % | +0.0 % | +0.0 % | 0.13 | 0.115 |
| AVIF | -24.8 % | -19.7 % | -16.4 % | -22.1 % | -17.4 % | 1.42 | 0.164 |
| JXL | -12.1 % | -13.4 % | +9.4 % | +4.4 % | +12.2 % | 0.19 | 0.146 |

Rangos de calidad sobre los que se integra el BD-rate: ssimulacra2: 30–90, ssimulacra2_hq: 70–90, psnr_y: 28–48, ssim_y: 0.85–0.995, psnr: 26–46.

![curvas](curves.png)
