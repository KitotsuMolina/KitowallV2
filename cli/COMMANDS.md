# Comandos exclusivos de Kitowall

Este inventario contiene solamente wallpapers estaticos. Excluye las familias historicas `live`, `we`, `host-setup` y cualquier comando de Kitsune.

## Contrato comun

```text
kitowall version
kitowall capabilities
kitowall status
kitowall doctor
kitowall config show
kitowall init
```

`init` solo crea o migra configuracion y datos propios. No instala paquetes, binarios ni unidades.

## Aplicacion y rotacion

```text
kitowall next [--pack <name>] [--force]
kitowall rotate-now [--pack <name>]
kitowall wallpaper list [--pack <name>] [--offset <n>] [--limit <n>]
kitowall wallpaper apply --pack <name> --id <id> --output <name>
kitowall wallpaper apply-batch --pack <name> --map <output:id,...>
kitowall mode <manual|rotate>
kitowall outputs
```

`outputs` es un alias de conveniencia sobre el contrato de `kitsune-compositor`; Kitowall no implementa deteccion nativa del escritorio.

Estado Rust 2026-07-16: los cuatro comandos estan implementados. `next` respeta modo manual, `rotate-now` fuerza la aplicacion, y ambos resuelven packs locales/remotos o el pool ponderado. Los providers remotos refrescan indices vacios e hidratan solo las imagenes seleccionadas antes de solicitar la aplicacion al compositor.

## Packs y providers

```text
kitowall pack list [--refresh] [--only-remote]
kitowall pack show <name>
kitowall pack status <name> [--refresh]
kitowall pack add <name> --type <type> [options]
kitowall pack update <name> [options]
kitowall pack remove <name>
kitowall pack refresh <name|--all> [--parallel]
kitowall pack hydrate <name> --count <n>
kitowall pack subtheme add <name> <value>
kitowall pack subtheme remove <name> <value>
kitowall pack set-key <name> [--api-key <key>|--api-key-env <env>]
kitowall pack-group add <name> --sources <list> [options]
```

Los nombres agrupados son el contrato objetivo. Durante la migracion se mantendran aliases para `list-packs`, `pack-status`, `refresh-pack` y `hydrate-pack`.

Estado Rust 2026-07-16: `pack list/show/add/update/remove/status/refresh/hydrate/set-key/subtheme` y `list-packs` estan disponibles. `add/update` soporta los seis providers. Los remotos generan indices, previews y descargas mediante HTTPS/Rustls con timeout, reintentos, limite de cuerpo y escritura atomica. `pack refresh --hydrate` combina ambos pasos.

Actualizacion 2026-07-26: Wallhaven y Unsplash comparten una sola credencial por
provider. Una clave suministrada mediante `pack add`, `pack update` o `pack set-key`
se guarda en `providerCredentials` y los packs futuros la reutilizan. Los campos
historicos por pack se migran cuando contienen una credencial consistente.

Actualizacion 2026-07-25: `pack list/show/status` ya ofrecen envelope
`--contract-v1` para el frontend. El catalogo visual usa `wallpaper list` paginado y la
aplicacion directa usa `wallpaper apply --pack <name> --id <id> --output <name>`.
`wallpaper apply-batch` valida todos los outputs e IDs antes de aplicar y solo confirma
estado e historial si todas las solicitudes al compositor terminan correctamente.

## Pool y cache

```text
kitowall pool list
kitowall pool status [--refresh]
kitowall pool enable
kitowall pool disable
kitowall pool add <name> [--weight <n>] [--max <n>]
kitowall pool remove <name>
kitowall cache config [options]
kitowall cache plan [--pack <name>]
kitowall cache prune [--pack <name>] --confirm
```

`--hard` conserva la semantica actual de eliminar descargas no protegidas, limitada a la raiz canonica del cache y respetando favoritos.

Estado Rust 2026-07-25: `cache status`, `cache plan` y `cache prune --confirm` estan
implementados. La poda respeta TTL, limite global y favoritos; solo mueve archivos
dentro de la raiz canonica administrada y los restaura si no puede confirmar el nuevo
indice. `--hard` no forma parte del nuevo contrato.

## Preferencias y observabilidad

```text
kitowall favorite list
kitowall favorite add <path>
kitowall favorite remove <path>
kitowall settings get
kitowall settings set [options]
kitowall transition set [options]
kitowall history list [--limit <n>]
kitowall history clear
kitowall logs list [filters]
kitowall logs clear
```

Los aliases actuales `favorites`, `transition`, `history` y `logs` se conservaran temporalmente para evitar una ruptura simultanea con la extraccion.

Estado Rust 2026-07-25: `transition set` esta implementado con `enabled`, `type`, `fps`, `duration`, `angle` y `pos`; `duration = 0` representa cambio inmediato. `mode <manual|rotate>` actualiza configuracion y estado. Favoritos, historial y logs persistentes con filtros estan implementados.

## Trabajos en segundo plano

```text
kitowall job start refresh <pack>
kitowall job start hydrate <pack> [--count <n>]
kitowall job list
kitowall job status <id>
kitowall job cancel <id>
```

Los jobs se guardan bajo el estado XDG. Hidratacion informa progreso por candidato y
atiende cancelacion antes de la siguiente descarga. Refresh puede cancelarse antes de
iniciar su peticion, pero una peticion HTTP ya iniciada termina antes de cambiar a
estado cancelado.

## Servicio propio

```text
kitowall service status
kitowall service start
kitowall service stop
kitowall service restart
kitowall service enable
kitowall service disable
kitowall service reschedule --every-seconds <n>
```

Estos comandos solicitan al compositor controlar unidades registradas de Kitowall. Crear, reparar, registrar o eliminar archivos de unidad tambien pertenece al compositor mediante descriptores tipados; GekkoApp solo distribuye los binarios y dependencias necesarias.

`service reschedule` actualiza solo la automatizacion `kitowall-next`. Si ya estaba
habilitada, reinicia su temporizador con el nuevo periodo; si no estaba instalada o
habilitada, conserva ese estado.

Estado Rust 2026-07-25: `service plan` y `service apply` declaran cuatro automatizaciones portatiles sin vocabulario systemd y las entregan en un unico lote. El compositor aplica sus cinco artefactos fisicos con rollback conjunto; la activacion se solicita despues mediante `service enable`. `watch` consulta outputs mediante el compositor y reaplica solo al cambiar su firma. `status/start/stop/restart/enable/disable/remove` operan el grupo logico mediante IDs exactos.

Actualizacion 2026-07-26: `service status --contract-v1` es tolerante a una
instalacion parcial. Siempre devuelve las cuatro automatizaciones logicas con estado
`active`, `enabled`, `stopped`, `not_installed` o `error`, sus artefactos fisicos y un
resumen agregado. Una automatizacion ausente ya no aborta el diagnostico completo.

## Comandos que salen de Kitowall

| Familia actual | Destino |
|---|---|
| `live ...` | Kilivepaper |
| `we ...` | fuera del backend estatico; soporte diferido |
| `host-setup ...` | GekkoApp |
| `install-systemd` / `uninstall-systemd` | GekkoApp; aliases no destructivos solo durante migracion |
| instalacion de `awww`, `swww` o paquetes | GekkoApp |
| deteccion nativa de outputs/escritorio | Compositor |

## Regla del CLI

El CLI valida argumentos y transforma respuestas al contrato comun. No selecciona wallpapers, descarga contenido, modifica indices ni ejecuta directamente `awww`; esas operaciones se invocan mediante interfaces del backend de Kitowall.
