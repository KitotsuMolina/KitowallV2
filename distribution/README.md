# Distribucion

Este directorio contiene el snapshot v1 del schema de artefactos consumido por el
workflow de release. La especificacion central es `docs/CONTRATO_DISTRIBUCION_V1.md`
en el workspace de desarrollo.

## Release local

Requisitos:

```text
Rust estable
jq
GNU tar
zstd
check-jsonschema
un generador SPDX como Syft
```

Ejemplo, despues de generar el SBOM:

```bash
cargo build --locked --release --target x86_64-unknown-linux-gnu \
  -p kitowall-cli

scripts/package-release.sh

scripts/generate-release-manifest.sh

check-jsonschema \
  --schemafile distribution/release-artifact-v1.schema.json \
  dist/kitowall-0.1.0-x86_64-unknown-linux-gnu.manifest.json
```

El generador inspecciona el binario con `readelf` y registra la version GLIBC mas
alta requerida realmente. El workflow `.github/workflows/release.yml` automatiza
el flujo completo al recibir un tag que coincida exactamente con la version de
`Cargo.toml`.
