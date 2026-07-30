# Kitowall

Producto propietario de wallpapers estaticos: biblioteca, providers, previews, aplicacion, rotacion y estado por output.

- `cli/`: comandos terminales de Kitowall.
- `backend/`: dominio y ejecucion de wallpapers estaticos.
- `frontend/`: reserva para superficies estaticas especializadas; la UI principal es
  el proyecto independiente KiUI.

Configuracion objetivo: `~/.config/kitowall/`.

El inventario inicial esta en `cli/COMMANDS.md` y los limites de dominio e integraciones en `backend/BOUNDARIES.md`.

## Distribucion

Los pushes y pull requests sobre `main` ejecutan formato, Clippy, pruebas y la
validacion del contrato CLI. Un tag semantico que coincida con la version del
workspace, por ejemplo `v0.1.0`, genera un archivo reproducible `tar.zst`, su
manifiesto de instalacion v1, SBOM SPDX, checksums y attestations, y los publica
en GitHub Releases. Consulta `distribution/README.md` para reproducir el
empaquetado localmente.

## Estado de migracion

El workspace Rust independiente ya existe. Migra configuracion, estado, packs tipados, los seis providers historicos, HTTPS, previews, cache, favoritos, historial y logs. `next`, `rotate-now`, aplicacion directa y lotes multi-output usan el contrato de `kitsune-compositor`. La poda valida rutas canonicas, protege favoritos y revierte si falla el indice. Refresh e hidratacion pueden ejecutarse como jobs persistentes con progreso y cancelacion cooperativa. El TypeScript original permanece solo como baseline de paridad hasta completar las pruebas reales y retirar la fachada antigua.
