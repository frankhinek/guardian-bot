{
  description = "Rust 1.90.0 + nightly rustfmt + rust-src; rust-analyzer from nixpkgs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Stable compiler pinned at 1.90.0.
        # Use the *minimal* profile so we don't pull in stable rustfmt/clippy.
        rust-stable = pkgs.rust-bin.stable."1.90.0".minimal.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
          ];
        };

        # Pick the latest nightly that *has* rustfmt available.
        rustfmt-nightly = pkgs.rust-bin.selectLatestNightlyWith (
          toolchain: toolchain.minimal.override { extensions = [ "rustfmt" ]; }
        );
      in
      {
        # Formatter used by `nix fmt`
        formatter = pkgs.nixfmt-rfc-style;

        devShells.default = pkgs.mkShell {
          packages = [
            rust-stable
            rustfmt-nightly
            pkgs.pkg-config
            pkgs.sqlite
            pkgs.taplo
          ];

          # Let IDEs and rust-analyzer find std sources
          RUST_SRC_PATH = "${rust-stable}/lib/rustlib/src/rust/library";
        };
      }
    );
}
