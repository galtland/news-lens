{
  description = "news-lens - CLI tool for wiki-grounded news commentary";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            # Rust toolchain
            rustToolchain

            # Build dependencies
            pkg-config
            openssl
            sqlite

            # Development tools
            cargo-watch
            cargo-edit
            cargo-outdated
            cargo-audit

            # GitHub CLI
            gh

            # Optional: for Ralph/Codex workflow
            nodejs_22
          ];

          shellHook = ''
            if [ -d .git ] && [ -d .githooks ]; then
              current_hooks_path=$(git config core.hooksPath || echo "")
              if [ "$current_hooks_path" != ".githooks" ]; then
                git config core.hooksPath .githooks
                echo "Git hooks configured (.githooks)"
                echo "  pre-commit: cargo fmt --check"
                echo "  pre-push:   cargo fmt + clippy + tests"
                echo "  Disable: git config --unset core.hooksPath"
              fi
            fi
          '';

          RUST_BACKTRACE = "1";
          RUST_LOG = "info";
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "news-lens";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ openssl sqlite ];
        };
      }
    );
}
