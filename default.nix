# Nix package definitions for Farhand, usable without flakes.
#
# The flake (flake.nix) is the primary interface; this exists so the packages
# can also be pulled in with plain `nix-build` / the nixpkgs overlay, which is
# how distro- and air-gapped setups usually consume things.
{ lib
, rustPlatform
, fetchgit
, fetchzip
, src ? null
, version ? null
}:

let
  # Default to the version in Cargo.toml, so the package definition and the
  # crate can never disagree.
  resolvedVersion = if version != null then version else (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

  # Prefer a local checkout (the flake passes `src = ./.`); fall back to the
  # released tarball so a pinned nixpkgs can still build a known version.
  source = if src != null then src else fetchgit {
    url = "https://github.com/Rayrsn/farhand.git";
    rev = "v${resolvedVersion}";
    sha256 = lib.fakeSha256;
  };

  common = {
    inherit source;
    cargoLock.lockFile = ./Cargo.lock;
    buildAndTestSubdir = null;
    doCheck = true;
  };
in
rec {
  fh = rustPlatform.buildRustPackage (common // {
    pname = "farhand-cli";
    version = resolvedVersion;
    meta = {
      description = "Farhand client: offload builds and tests to a remote machine";
      homepage = "https://github.com/Rayrsn/farhand";
      license = lib.licenses.mit;
      mainProgram = "fh";
      platforms = lib.platforms.unix ++ lib.platforms.windows;
    };
  });

  fhd = rustPlatform.buildRustPackage (common // {
    pname = "farhand-agent";
    version = resolvedVersion;
    meta = {
      description = "Farhand agent: runs authenticated commands in a persistent remote workspace";
      homepage = "https://github.com/Rayrsn/farhand";
      license = lib.licenses.mit;
      mainProgram = "fhd";
      platforms = lib.platforms.unix;
    };
  });
}
