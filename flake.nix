{
  description = "Terminal UI for browsing OPDS e-book catalogs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      packages = forAllSystems (
        pkgs:
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "opdsview";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.darwin.apple_sdk.frameworks.Security
              pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
            ];

            meta = {
              description = "Terminal UI for browsing OPDS e-book catalogs";
              homepage = "https://github.com/ahacop/opdsview";
              license = pkgs.lib.licenses.gpl3Plus;
              mainProgram = "opdsview";
            };
          };
        }
      );

      devShells = forAllSystems (
        pkgs:
        let
          rust = pkgs.rust-bin.stable.latest.default.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
              "clippy"
              "rustfmt"
            ];
          };
          # The flake is the source of truth for the cargo-dist version: the dev
          # shell ships whatever cargo-dist the pinned nixpkgs provides, and CI
          # installs the version pinned in dist-workspace.toml. They must match,
          # so warn on entry if a flake.lock bump has let them drift.
          flakeDistVersion = pkgs.cargo-dist.version;
          pinnedDistVersion =
            (builtins.fromTOML (builtins.readFile ./dist-workspace.toml)).dist.cargo-dist-version;
        in
        {
          default = pkgs.mkShell {
            packages = [
              rust
              pkgs.bacon
              pkgs.cargo-dist
              pkgs.cargo-edit
              pkgs.cargo-nextest
              pkgs.just
            ];

            RUST_BACKTRACE = 1;

            shellHook = pkgs.lib.optionalString (flakeDistVersion != pinnedDistVersion) ''
              echo "⚠ cargo-dist drift: flake provides ${flakeDistVersion}, but dist-workspace.toml pins ${pinnedDistVersion}." >&2
              echo "  Set cargo-dist-version = \"${flakeDistVersion}\" in dist-workspace.toml and run 'dist generate'." >&2
            '';
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
