{
  description = "hikyaku (飛脚) — node agent of the observability stack: scrapes, logs, durable spool, push";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";

    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    crane.url = "github:ipetkov/crane";

    # The stores: their protos are compiled here, their fixtures run against this agent's
    # validators (ADR 0001, 0008). Locally: `--override-input masu path:../masu` (same for nisshi).
    masu = {
      url = "git+https://kajiba.uchi.scorsi.dev/scorsi/masu.git";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-parts.follows = "flake-parts";
      inputs.treefmt-nix.follows = "treefmt-nix";
      inputs.crane.follows = "crane";
    };
    nisshi = {
      url = "git+https://kajiba.uchi.scorsi.dev/scorsi/nisshi.git";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-parts.follows = "flake-parts";
      inputs.treefmt-nix.follows = "treefmt-nix";
    };
  };

  outputs =
    inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];

      imports = [
        inputs.treefmt-nix.flakeModule
        ./nix/modules.nix
        ./nix/perSystem.nix
      ];
    };
}
