{inputs, ...}: {
  perSystem = {
    pkgs,
    lib,
    ...
  }: let
    package = builtins.fromTOML (builtins.readFile ../src-tauri/Cargo.toml);
    toolchainManifest = builtins.fromTOML (builtins.readFile ../rust-toolchain.toml);
    rusttoolchain = (pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml).override {
      extensions = toolchainManifest.toolchain.components ++ ["clippy"];
    };

    craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rusttoolchain;
    cargoSource = lib.cleanSource ../src-tauri;

    libraries = with pkgs; [
      webkitgtk_4_1
      gtk3
      glib
      dbus
      openssl
      librsvg
      libsoup_3
    ];

    commonArgs = {
      pname = package.package.name;
      inherit (package.package) version;
      src = cargoSource;
      cargoVendorDir = craneLib.vendorCargoDeps {
        cargoLock = ../src-tauri/Cargo.lock;
      };
      strictDeps = true;

      nativeBuildInputs = with pkgs; [pkg-config];
      buildInputs = libraries;
    };

    cargoArtifacts = craneLib.buildDepsOnly commonArgs;
  in {
    checks.clippy = craneLib.cargoClippy (commonArgs
      // {
        inherit cargoArtifacts;
        cargoClippyExtraArgs = "--all-targets -- --deny warnings";
      });
  };
}
