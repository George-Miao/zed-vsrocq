{
  description = "VsRocq for Zed";

  inputs = {
    nixpkgs.url = "nixpkgs";
    nixpkgs-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
    devshell = {
      url = "github:numtide/devshell";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      nixpkgs-darwin,
      devshell,
      fenix,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      eachSystem = nixpkgs.lib.genAttrs systems;
      packagesFor =
        system:
        import (if system == "x86_64-darwin" then nixpkgs-darwin else nixpkgs) {
          inherit system;
        };
    in
    {
      devShells = eachSystem (
        system:
        let
          pkgs = packagesFor system;
          rustPkgs = import fenix { inherit system pkgs; };
          rust = rustPkgs.combine [
            (rustPkgs.stable.withComponents [
              "cargo"
              "rustc"
              "rust-std"
              "rust-src"
              "rustfmt"
              "clippy"
            ])
            rustPkgs.targets.wasm32-wasip2.stable.rust-std
          ];
        in
        {
          default = (import devshell { nixpkgs = pkgs; }).mkShell {
            name = "vsrocq";
            packages = [
              rust
              pkgs.stdenv.cc
              pkgs.pkg-config
              pkgs.rocq-core
              pkgs.rocqPackages.stdlib
              pkgs.rocqPackages.vsrocq-language-server
              pkgs.bun
              pkgs.nixfmt
            ];
            commands = [
              {
                name = "build-bridge";
                help = "Build the local proof bridge";
                command = "cargo build -p vsrocq-bridge";
              }
              {
                name = "build-extension";
                help = "Build the Zed extension";
                command = "cargo build -p zed-vsrocq --target wasm32-wasip2 --release";
              }
              {
                name = "check-project";
                help = "Check TypeScript, Rust, and tests";
                command = "bun run check && cargo test --workspace && cargo check -p zed-vsrocq --target wasm32-wasip2";
              }
            ];
          };
        }
      );
      formatter = eachSystem (system: (packagesFor system).nixfmt);
    };
}
