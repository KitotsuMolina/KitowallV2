# Kitowall Backend

Logica de biblioteca estatica, providers, previews, seleccion, historial, aplicacion y rotacion.

El workspace Rust inicial ya contiene configuracion JSON v1 compatible con TypeScript, los seis tipos de pack, validacion, estado persistente, cooldowns y seleccion multi-output. Tambien incluye CRUD de catalogo y el provider local recursivo con previews de imagen schema v1.

Los seis providers historicos estan migrados: `local`, `static_url`, `wallhaven`, `reddit`, `unsplash` y `generic_json`. Los remotos generan indices y previews compatibles y pueden hidratar mediante un `HttpTransport` inyectable con limite de 100 MiB, escritura temporal, rename atomico, validacion de pertenencia del candidato y rollback si falla el indice de cache.

El transporte de produccion usa `ureq` 3 con Rustls, timeout global de 30 segundos, conexion de 5 segundos, cuerpo de 20 segundos, maximo de cinco redirecciones y dos reintentos para fallos/transitorios HTTP. Solo acepta URL `http://` o `https://`; si el servidor declara `Content-Type`, debe ser `image/*`.

El indice de cache historico puede leerse y escribirse atomicamente; TTL, limite de tamano y favoritos producen un plan puro de poda. La aplicacion destructiva de ese plan permanece deshabilitada hasta completar pruebas de seguridad adicionales.

El resolvedor de biblioteca combina packs locales/remotos, limites, pesos y deduplicacion del pool. El controlador obtiene outputs mediante un puerto, aplica cooldowns, hidrata solamente los candidatos elegidos y confirma el estado despues de aplicar todos los outputs. El CLI implementa ese puerto consumiendo exclusivamente el contrato publico del compositor.

Favoritos e historial conservan los esquemas JSON anteriores, usan escritura atomica y permisos privados. Cada aplicacion exitosa agrega una entrada por output; las consultas se ordenan de mas reciente a mas antigua y marcan favoritos.

Logs y servicios de rotacion se migraran en bloques posteriores. La aplicacion multi-output usa actualmente una llamada del compositor por output: ante un fallo no se confirma el estado, aunque el renderer podria haber aplicado outputs anteriores. Un contrato batch futuro permitira atomicidad visual o rollback explicito.

No instala paquetes, no escribe unidades de servicio y no ejecuta herramientas del escritorio. Esas operaciones se solicitan mediante el contrato publico de `kitsune-compositor`.

## Pruebas locales

```text
cargo test --workspace
cargo run -p kitowall-cli -- config show
cargo run -p kitowall-cli -- status
cargo run -p kitowall-cli -- outputs
```

Para no tocar datos reales durante pruebas manuales:

```text
XDG_CONFIG_HOME=/tmp/kitowall-config XDG_STATE_HOME=/tmp/kitowall-state cargo run -p kitowall-cli -- config init
```
