# Resultados del banco de pruebas

DIV2K: 50 de las 100 fotos de validación (2,7 MP). Ninguna se usó para ajustar ni entrenar Tesela.

Imágenes: 50. BD-rate frente a **WebP** (negativo = menos bytes a igual calidad).

| Códec | SSIMULACRA2 | SSIMULACRA2 alta calidad | PSNR-Y | SSIM-Y | PSNR RGB | Cod. (s/img) | Dec. (s/img) |
|---|---|---|---|---|---|---|---|
| TESELA | -34.0 % | -30.5 % | -24.1 % | -30.9 % | -29.3 % | 13.72 | 0.699 |
| WEBP | +0.0 % | +0.0 % | +0.0 % | +0.0 % | +0.0 % | 0.91 | 0.911 |
| AVIF | -32.5 % | -27.3 % | -22.3 % | -29.4 % | -23.2 % | 11.89 | 1.710 |
| JXL | -20.3 % | -21.0 % | +2.7 % | -4.7 % | +7.1 % | 1.05 | 0.851 |

Rangos de calidad sobre los que se integra el BD-rate: ssimulacra2: 30–90, ssimulacra2_hq: 70–90, psnr_y: 28–48, ssim_y: 0.85–0.995, psnr: 26–46.

![curvas](curves.png)
