# Resultados del banco de pruebas

Imágenes: 9. BD-rate frente a **WebP** (negativo = menos bytes a igual calidad).

| Códec | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Cod. (s/img) | Dec. (s/img) |
|---|---|---|---|---|---|---|---|
| TESELA | -60.1 % | -63.3 % | -55.5 % | -58.5 % | -60.5 % | 4.97 | 0.144 |
| WEBP | +0.0 % | +0.0 % | +0.0 % | +0.0 % | +0.0 % | 0.21 | 0.124 |
| AVIF | -54.3 % | -57.3 % | -47.1 % | -51.0 % | -52.3 % | 4.72 | 0.150 |
| JXL | -36.0 % | -36.0 % | -2.4 % | -5.3 % | -12.4 % | 0.60 | 0.207 |

Rangos de calidad sobre los que se integra el BD-rate: ssimulacra2: 30–90, ssimulacra2_hq: 70–90, psnr_y: 28–48, ssim_y: 0.85–0.995, psnr: 26–46.

![curvas](curves.png)
