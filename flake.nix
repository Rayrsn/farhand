{
  description = "Farhand — remote build and test offloader with zero external system binaries";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        rustToolchain = pkgs.rustPlatform.rustc;
        # One source of truth: the version lives in Cargo.toml and nowhere else.
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      in
      {
        packages = {
          default = self.packages.${system}.fh;

          fh = pkgs.rustPlatform.buildRustPackage {
            pname = "farhand-cli";
            inherit version;

            src = ./.;

            cargoLock.lockFile = ./Cargo.lock;

            # The workspace is a library tree with two binaries at its root.
            buildAndTestSubdir = null;
            doCheck = true;

            meta = with pkgs.lib; {
              description = "Farhand client: offload compilation, test, and build jobs to a remote machine";
              homepage = "https://github.com/Rayrsn/farhand";
              license = licenses.mit;
              mainProgram = "fh";
              platforms = platforms.unix ++ platforms.windows;
            };
          };

          fhd = pkgs.rustPlatform.buildRustPackage {
            pname = "farhand-agent";
            inherit version;

            src = ./.;

            cargoLock.lockFile = ./Cargo.lock;

            buildAndTestSubdir = null;
            doCheck = true;

            meta = with pkgs.lib; {
              description = "Farhand agent: executes authenticated commands in a persistent remote workspace";
              homepage = "https://github.com/Rayrsn/farhand";
              license = licenses.mit;
              mainProgram = "fhd";
              platforms = platforms.unix;
            };
          };
        };

        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.cargo
            pkgs.rustfmt
            pkgs.clippy
            pkgs.rust-analyzer
            pkgs.cargo-audit
            pkgs.cargo-deny
            pkgs.protobuf
          ];
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          shellHook = ''
            echo "farhand dev shell — cargo fmt/clippy/test, plus cargo-audit and cargo-deny"
          '';
        };

        # `nix flake check` builds both packages.
        checks = {
          inherit (self.packages.${system}) fh fhd;
        };

        overlays.default = final: prev: {
          farhand = final.callPackage ./default.nix { };
        };

        formatter = pkgs.nixpkgs-fmt;
      }
    );
}
