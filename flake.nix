{
  description = "Shakespeare AI: an OpenAI-compatible AI provider with Nostr auth and prepaid credits";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "shakespeare-ai";
          version = "0.1.0";
          src = nixpkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: _type: baseNameOf path != "target";
          };
          cargoLock.lockFile = ./Cargo.lock;
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.gcc pkgs.postgresql ];
        };
      });
    };
}
