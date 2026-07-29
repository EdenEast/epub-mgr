{inputs, ...}: {
  perSystem = {
    pkgs,
    lib,
    ...
  }: let
    package = builtins.fromTOML (builtins.readFile ../src-tauri/Cargo.toml);
    toolchainManifest = builtins.fromTOML (builtins.readFile ../rust-toolchain.toml);
    rusttoolchain = (pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml).override {
      extensions = toolchainManifest.toolchain.components;
    };

    craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rusttoolchain;

    source = lib.cleanSourceWith {
      src = lib.cleanSource ../.;
      filter = path: _type: let
        baseName = baseNameOf path;
      in
        !(lib.elem baseName [
          "node_modules"
          "target"
          "dist"
        ]);
    };

    cargoSource = lib.cleanSource ../src-tauri;

    frontend = pkgs.buildNpmPackage {
      pname = "${package.package.name}-frontend";
      inherit (package.package) version;
      src = source;

      npmDepsHash = "sha256-HrFsHG0kPlDNsTN30km7/zOcx0gzqfdmSl3zcDbh0Mk=";
      npmBuildScript = "build:web";

      installPhase = ''
        runHook preInstall
        mkdir -p $out
        cp -r dist $out/dist
        runHook postInstall
      '';
    };

    libraries = with pkgs; [
      webkitgtk_4_1
      gtk3
      glib
      dbus
      openssl
      librsvg
      libsoup_3
    ];

    cargoVendorDir = craneLib.vendorCargoDeps {
      cargoLock = ../src-tauri/Cargo.lock;
    };

    cargoArtifacts = craneLib.buildDepsOnly {
      pname = package.package.name;
      inherit (package.package) version;
      src = cargoSource;
      inherit cargoVendorDir;
      strictDeps = true;

      nativeBuildInputs = with pkgs; [pkg-config];
      buildInputs = libraries;
    };

    app = craneLib.buildPackage {
      pname = package.package.name;
      inherit (package.package) version;
      src = cargoSource;

      inherit cargoVendorDir cargoArtifacts;
      strictDeps = true;

      nativeBuildInputs = with pkgs; [
        pkg-config
        wrapGAppsHook3
      ];

      buildInputs = libraries;

      postPatch = ''
        cp -r ${frontend}/dist ./dist
        substituteInPlace tauri.conf.json \
          --replace-fail '"frontendDist": "../dist"' '"frontendDist": "dist"'
      '';

      postInstall = ''
        install -Dm644 icons/icon.png \
          $out/share/icons/hicolor/512x512/apps/epub-mgr.png

        install -Dm644 /dev/stdin $out/share/applications/epub-mgr.desktop <<'EOF'
        [Desktop Entry]
        Type=Application
        Name=EPUB Manager
        Comment=Clean and normalize a personal EPUB library
        Exec=epub-mgr
        Icon=epub-mgr
        Terminal=false
        Categories=Office;Utility;
        EOF
      '';

      meta = {
        description = package.package.description;
        mainProgram = package.package.name;
        platforms = lib.platforms.linux;
      };
    };
  in {
    packages.${package.package.name} = app;
    packages.default = app;
  };
}
