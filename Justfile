manifest := "software/deimos/Cargo.toml"

default:
    @just --list

build:
    cargo build --manifest-path {{manifest}}

test:
    cargo test --manifest-path {{manifest}}

example name:
    cargo build --example {{name}} --manifest-path {{manifest}}

hootl:
    RUST_LOG=info cargo run --example hootl_lifecycle --manifest-path {{manifest}}

hootl-butter-reset:
    RUST_LOG=warn cargo run --example hootl_butter_reset --manifest-path {{manifest}}

hootl-csv-fallback:
    RUST_LOG=warn DEIMOS_DB_PW=ignored cargo run --example hootl_csv_fallback --manifest-path {{manifest}}

fmt:
    cargo fmt --manifest-path {{manifest}}

clippy:
    cargo clippy --manifest-path {{manifest}} --all-targets -- -D warnings
