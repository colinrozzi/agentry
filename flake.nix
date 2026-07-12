{
  description = "agentry: a CLI tool for managing local AI agent sessions";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable.latest.default;

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        commonArgs = {
          # Keep Cargo/Rust sources *plus* the embedded assets under src/assets
          # (recipe.toml, CLAUDE.md, Dockerfile) that `include_str!` needs.
          # `cleanCargoSource` alone strips them and the release build fails.
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              (craneLib.filterCargoSources path type)
              || (builtins.match ".*/src/assets/.*" path != null);
          };
          pname = "agentry";
          version = "0.1.0";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      in {
        packages.default = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });

        packages.clippy = craneLib.cargoClippy (commonArgs // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "-- -D warnings";
        });

        packages.fmt = craneLib.cargoFmt {
          inherit (commonArgs) src pname version;
        };

        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain pkgs.ripgrep pkgs.tmux ];
          shellHook = ''
            echo "agentry dev environment"
            echo "  cargo build"
            echo "  cargo run -- recipes list"
          '';
        };
      });
}
