{
  description = "Development shell for the EPUB Manager Tauri app";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = {nixpkgs, ...}: let
    systems = ["x86_64-linux" "aarch64-linux"];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    devShells = forAllSystems (system: let
      pkgs = import nixpkgs {inherit system;};
      libraries = with pkgs; [
        webkitgtk_4_1
        gtk3
        glib
        dbus
        openssl
        librsvg
        libsoup_3
      ];
    in {
      default = pkgs.mkShell {
        packages = with pkgs; [
          cargo
          rustc
          rustfmt
          clippy
          just
          nodejs_22
          pkg-config
          glib-networking
          libsoup_3
          webkitgtk_4_1
          gtk3
          openssl
          librsvg
          makeWrapper
        ];

        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libraries;
        PKG_CONFIG_PATH = pkgs.lib.makeSearchPath "lib/pkgconfig" libraries;

        shellHook = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
          # GTK's native file chooser reads schemas from XDG_DATA_DIRS. The
          # regular package share paths are not enough on Nix because compiled
          # schemas live under share/gsettings-schemas/<package-name>.
          export XDG_DATA_DIRS="${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}:${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:''${XDG_DATA_DIRS:-}"

          if grep -qiE '(microsoft|wsl)' /proc/sys/kernel/osrelease /proc/version 2>/dev/null; then
            # WSLg can expose an EGL stack that WebKitGTK cannot initialize from
            # inside the Nix shell. Point libglvnd at Nixpkgs Mesa explicitly so
            # LIBGL_ALWAYS_SOFTWARE reliably gets llvmpipe instead of failing
            # before the software renderer is found.
            export WEBKIT_DISABLE_DMABUF_RENDERER=1
            export LIBGL_ALWAYS_SOFTWARE=1
            export MESA_LOADER_DRIVER_OVERRIDE=llvmpipe
            export GALLIUM_DRIVER=llvmpipe
            export LIBGL_DRIVERS_PATH="${pkgs.mesa}/lib/dri''${LIBGL_DRIVERS_PATH:+:$LIBGL_DRIVERS_PATH}"
            export LD_LIBRARY_PATH="${pkgs.mesa}/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            export __EGL_VENDOR_LIBRARY_FILENAMES="${pkgs.mesa}/share/glvnd/egl_vendor.d/50_mesa.json''${__EGL_VENDOR_LIBRARY_FILENAMES:+:$__EGL_VENDOR_LIBRARY_FILENAMES}"
          fi
        '';
      };
    });
  };
}
