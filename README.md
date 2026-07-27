# Kitowall

Producto propietario de wallpapers estaticos: biblioteca, providers, previews, aplicacion, rotacion y estado por output.

- `cli/`: comandos terminales de Kitowall.
- `backend/`: dominio y ejecucion de wallpapers estaticos.
- `frontend/`: reserva para superficies estaticas especializadas; la UI principal es
  el proyecto independiente KiUI.

Configuracion objetivo: `~/.config/kitowall/`.

El inventario inicial esta en `cli/COMMANDS.md` y los limites de dominio e integraciones en `backend/BOUNDARIES.md`.

## Estado de migracion

El workspace Rust independiente ya existe. Migra configuracion, estado, packs tipados, los seis providers historicos, HTTPS, previews, cache, favoritos, historial y logs. `next`, `rotate-now`, aplicacion directa y lotes multi-output usan el contrato de `kitsune-compositor`. La poda valida rutas canonicas, protege favoritos y revierte si falla el indice. Refresh e hidratacion pueden ejecutarse como jobs persistentes con progreso y cancelacion cooperativa. El TypeScript original permanece solo como baseline de paridad hasta completar las pruebas reales y retirar la fachada antigua.
