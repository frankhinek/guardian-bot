# Justfile for guardian-bot-new
# Run `just --list` to see all available commands

# Default recipe - show available commands
default:
    @just --list

# Run both format and lint checks
check-all: format-check lint
    @echo "✓ All checks complete"

# Format all code in the workspace
format:
    @echo "Formatting TOML files..."
    taplo format
    @echo "Formatting Rust code..."
    cargo fmt --all
    @echo "Formatting Nix files..."
    find . -name '*.nix' -not -path '*/target/*' -not -path '*/.direnv/*' -exec nix fmt {} \;
    @echo "✓ All formatting complete"

# Check if code is formatted without making changes
format-check:
    @echo "Checking TOML formatting..."
    taplo fmt --check
    @echo "Checking Rust formatting..."
    cargo fmt --all --check
    @echo "✓ Format check complete"

# Run linter (clippy) with warnings
lint:
    @echo "Running clippy..."
    cargo clippy --all-targets --all-features -- -D warnings
    @echo "✓ Clippy checks complete"

# Run all tests in the workspace
test:
    @echo "Running tests..."
    cargo test --all-features --workspace
    @echo "✓ All tests complete"
