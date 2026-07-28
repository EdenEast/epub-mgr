set dotenv-load := true

# List available recipes
_default:
    @just --list

# Install frontend dependencies
install:
    npm install

# Run the Tauri app in development mode
dev:
    npm run dev

# Build the frontend only
build-web:
    npm run build:web

# Type/check the Rust backend
cargo-check:
    cd src-tauri && cargo check

# Format Rust and frontend files
fmt:
    cd src-tauri && cargo fmt
    npx prettier --write index.html 'src/**/*.{js,css}' package.json

# Run the useful fast checks
check: build-web cargo-check

# Build the distributable Tauri app
build:
    npm run build

# Remove generated build outputs
clean:
    rm -rf dist src-tauri/target
