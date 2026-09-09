{
  description = "sshr - Resilient SSH sessions with automatic reconnection";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    nixpkgs,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {inherit system;};
    in {
      packages.default = pkgs.rustPlatform.buildRustPackage {
        pname = "sshr";
        version = "0.1.0";
        src = ./.;
        cargoHash = "sha256-kz/BAOnP+hj3gfXoTE7zNLORzqTC0+1iMTLIAXE750A=";

        postInstall = ''
          mkdir -p $out/share/sshr/{kitty,shpool}
          cp kitty/*.py $out/share/sshr/kitty/
          cp shpool/build.sh $out/share/sshr/shpool/
          if [ -d shpool/bin ]; then
            cp -r shpool/bin $out/share/sshr/shpool/
          fi
        '';

        meta = with pkgs.lib; {
          description = "Resilient SSH sessions with automatic reconnection";
          license = licenses.mit;
          platforms = platforms.unix;
          mainProgram = "sshr";
        };
      };

      devShells.default = pkgs.mkShell {
        buildInputs = with pkgs; [cargo rustc rust-analyzer clippy rustfmt];
      };
    });
}
