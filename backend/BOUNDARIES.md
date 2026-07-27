# Limites del backend de Kitowall

## Por que existe aunque se use awww

`awww` y `swww` son renderers de imagen: reciben una ruta, un output y parametros de transicion. No conocen packs, providers remotos, cache, favoritos, historial, pool, rotacion, estado persistente ni politicas de seleccion.

Por tanto Kitowall necesita un backend propio que resuelva el dominio estatico y entregue al renderer una orden final como:

```text
ApplyStaticWallpaper {
  output,
  image_path,
  transition,
  namespace
}
```

## Modulos del backend

| Modulo | Responsabilidad |
|---|---|
| configuracion | schema, validacion y migraciones XDG |
| providers | Reddit, Wallhaven, Unsplash, JSON generico, URL estatica y carpetas locales |
| biblioteca/cache | indices, descargas atomicas, limites, TTL y borrado seguro |
| packs/pool | catalogo, pesos, subtemas y combinacion de candidatos |
| seleccion | deduplicacion y eleccion por output |
| favoritos/historial/logs | persistencia propia y consultas |
| rotacion | modo manual/rotate, siguiente seleccion y temporizacion funcional |
| aplicacion | coordinacion transaccional con outputs y renderer estatico |
| diagnostico | salud del dominio y lectura de capacidades externas |

## Puertos externos

El backend no debe ejecutar APIs nativas de Hyprland, Niri, GNOME o KDE. Consumira interfaces explicitas:

```text
CompositorPort
  detect()
  outputs()
  focused_output()
  validate_output(name)
  capabilities()

StaticWallpaperRendererPort
  capabilities()
  status(namespace)
  start(namespace)
  apply(request)
  stop(namespace)
```

En la primera implementacion, ambos puertos invocan contratos de `kitsune-compositor`. Este selecciona el adapter de escritorio y el adapter `awww`; `swww` puede conservarse para compatibilidad.

## Instalacion frente a ejecucion

La instalacion y la ejecucion son operaciones distintas:

- GekkoApp instala el paquete/binario `awww` y el binario `kitowall` en las rutas elegidas.
- El compositor informa escritorio, outputs y capacidades, ejecuta el renderer ya instalado y materializa/controla servicios desde descriptores validados.
- Kitowall envia solicitudes abstractas de aplicacion y runtime; no ejecuta `awww` directamente.

El compositor no instala paquetes. Registra bajo su propio namespace cada archivo de servicio que materializa y ofrece la operacion simetrica para retirarlo. GekkoApp no participa en este flujo.

## Dependencias prohibidas

- Codigo o indices de Kilivepaper.
- Captura, perfiles o renderizado de Kitsune.
- Gestores de paquetes (`pacman`, `apt`, `dnf`, `zypper`) y `sudo`.
- Llamadas directas a `hyprctl` o `niri msg`.
- Creacion o eliminacion de unidades fuera del registro exacto del compositor.
