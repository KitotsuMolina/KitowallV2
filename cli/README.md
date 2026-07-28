# Kitowall CLI

Aqui se trasladaran los comandos publicos de wallpapers estaticos. Esta capa parsea argumentos y presenta resultados; la logica se ejecuta en el backend de Kitowall.

Binario objetivo: `kitowall`.

## Comandos Rust disponibles

```text
kitowall version
kitowall capabilities
kitowall doctor [--contract-v1]
kitowall status
kitowall config show
kitowall config init
kitowall outputs
kitowall wallpaper list [--pack <name>] [--offset <n>] [--limit <1-200>] [--contract-v1]
kitowall wallpaper apply --pack <name> --id <id> --output <name> [--namespace <name>] [--contract-v1]
kitowall dashboard snapshot [--pack <name>] [--contract-v1]
kitowall mode <manual|rotate>
kitowall settings get [--contract-v1]
kitowall settings set --rotation-interval-seconds <n> [--contract-v1]
kitowall transition set [--enabled <bool>] [--type <type>] [--fps <n>] [--duration <n>] [--angle <n>] [--pos <x,y>]
kitowall next [--pack <name>] [--force] [--namespace <name>]
kitowall rotate-now [--pack <name>] [--namespace <name>]
kitowall favorite list
kitowall favorite add <path>
kitowall favorite remove <path>
kitowall favorites
kitowall history [list] [--limit <n>]
kitowall history clear
kitowall watch [--poll-ms <n>] [--namespace <name>]
kitowall service plan [--every-seconds <n>] [--namespace <name>]
kitowall service apply [--every-seconds <n>] [--namespace <name>]
kitowall service reschedule [--every-seconds <n>] [--namespace <name>]
kitowall service status|start|stop|restart|enable|disable|remove
kitowall pack list
kitowall pack show <name>
kitowall pack add <name> --type local --paths <lista>
kitowall pack update <name> --type local --paths <lista>
kitowall pack remove <name>
kitowall pack status <name>
kitowall pack refresh <name>
kitowall pack hydrate <name> [--count <n>]
kitowall pack refresh <name> --hydrate [--count <n>]
kitowall pack set-key <name> [--api-key <key>|--api-key-env <env>]
kitowall pack subtheme <add|remove> <name> <value>
kitowall list-packs
kitowall cache status
kitowall cache plan [--pack <name>]
```

`pack status`, `refresh` e `hydrate` soportan `static_url`, Wallhaven, Reddit, Unsplash y JSON generico. `pack hydrate` exige un indice previo y limita cada invocacion a 100 candidatos. Wallhaven y Unsplash admiten token directo o variable de entorno. La credencial se almacena una sola vez por provider en `providerCredentials` y se reutiliza en todos sus packs; `pack set-key` conserva su sintaxis por compatibilidad, pero actualiza la credencial compartida. Las salidas nunca muestran el valor directo.

`outputs`, `next` y `rotate-now` requieren `kitsune-compositor`. Los builds debug
lo buscan por defecto en el workspace local; con `--lc`, el mismo modo puede
forzarse en cualquier build. Kitowall busca
`../compositor/target/{debug|release}/kitsune-compositor` desde la raiz
`refactor/` y propaga el modo local. Durante pruebas, un binario indicado mediante
`KITSUNE_COMPOSITOR_BIN` mantiene prioridad. Kitowall no ejecuta `awww`, `swww`,
`hyprctl` ni `niri` directamente.

Cuando `next` o `rotate-now` se ejecutan sin `--pack`, la rotacion avanza en
round-robin por todos los packs configurados. Si existe un pool habilitado se usa
su mezcla ponderada. `--pack <name>` es la unica forma de fijar una ejecucion a un
pack concreto.

Un build `--release` sin `--lc` resuelve `kitsune-compositor` mediante `PATH`.

`wallpaper list` es el primer contrato dedicado al frontend: pagina un catalogo normalizado sin hacer red ni descargar archivos. Conserva previews, metadata, favorito, hidratacion y outputs activos. `wallpaper apply` solo acepta un ID emitido para el pack indicado, valida el output mediante el compositor, hidrata bajo demanda y persiste estado/historial despues de aplicar correctamente.

`dashboard snapshot` entrega en una sola lectura el catalogo, facetas, packs
sanitizados, jobs, historial, contadores `available/downloaded/local`, una revision
determinista y las rutas de dominio que un frontend local puede observar. No consulta
ni delega esta informacion al compositor.

`version`, `capabilities`, `doctor`, `status`, `config show`, `outputs`, settings, modo, transiciones, `wallpaper list/apply`, packs, favoritos, historial, `cache status/plan` y servicios aceptan `--contract-v1`. Siguen pendientes para el gate `next/rotate-now`, logs, poda mutable, jobs de progreso y aplicacion directa multi-output.

Una transicion con `--enabled false` o `--duration 0` aplica el cambio sin animacion. Kitowall admite `simple`, `fade`, `left`, `right`, `top`, `bottom`, `wipe`, `wave`, `grow`, `center`, `outer`, `any` y `random`.

`favorites` conserva el alias historico de `favorite list`. `history --limit <n>` tambien se mantiene como alias de `history list --limit <n>`. Los archivos compatibles se guardan bajo `XDG_STATE_HOME/kitowall/` o `~/.local/state/kitowall/`.

`service plan/apply` genera cuatro intenciones portatiles (`runtime`, `next`, `watch` y `login-apply`) y las entrega en una unica solicitud batch al compositor. No construye nombres de unidades ni targets. Bajo `systemd-user` estas intenciones producen cinco artefactos porque `next` requiere servicio y timer. `apply` materializa el lote con rollback conjunto, pero no lo activa; despues debe ejecutarse `kitowall service enable`. Las acciones de control operan los cuatro IDs logicos; apagado, deshabilitacion y retiro usan orden inverso.

Ejemplo remoto sin descarga:

```text
kitowall pack add demo --type static_url --urls https://host/a.jpg,https://host/b.png --different-images true --count 2
kitowall pack refresh demo
kitowall pack status demo
kitowall pack hydrate demo --count 1
```

La matriz completa de opciones por provider esta en `../../../docs/MATRIZ_CONFIG_PROVIDERS_KITOWALL.md`.

Durante desarrollo no es necesario instalar el binario:

```text
cargo run -p kitowall-cli -- pack list
cargo run -p kitowall-cli -- transition set --type wipe --duration 0.7
cargo run -p kitowall-cli -- --lc outputs --contract-v1
```

La linea anterior es sintaxis del shell para lanzar Cargo; el CLI y su backend estan implementados en Rust, no en Bash.

`cache plan` siempre devuelve `dry_run: true`. No borra archivos ni modifica el indice.
