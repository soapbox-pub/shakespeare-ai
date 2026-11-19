{
  description = "Shakespeare AI: an OpenAI-compatible AI provider with Nostr auth and prepaid credits";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        # The SPA, built on its own so the server can compile it in.
        dashboard = pkgs.buildNpmPackage {
          pname = "shakespeare-ai-dashboard";
          version = "0.1.0";
          src = nixpkgs.lib.cleanSourceWith {
            src = ./dashboard;
            filter = path: _type: !(builtins.elem (baseNameOf path) [ "node_modules" "dist" ]);
          };
          # Refresh with `prefetch-npm-deps dashboard/package-lock.json` when
          # the dashboard's dependencies change.
          npmDepsHash = "sha256-WCMyUHMtEdBLTMb0+Z3K7HT44NvJTXUQZcbl7sJykKc=";
          installPhase = "cp -r dist $out";
        };

        default = pkgs.rustPlatform.buildRustPackage {
          pname = "shakespeare-ai";
          version = "0.1.0";
          src = nixpkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: _type: !(builtins.elem (baseNameOf path) [ "target" "dashboard" ]);
          };
          cargoLock.lockFile = ./Cargo.lock;
          buildFeatures = [ "embedded-dashboard" ];
          preBuild = ''
            mkdir -p dashboard
            cp -r ${dashboard} dashboard/dist
          '';
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.gcc pkgs.nodejs pkgs.postgresql ];
        };
      });
    };
}
