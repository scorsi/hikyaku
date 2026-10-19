# nixosModules.default and darwinModules.default (ADR 0007): `services.hikyaku` on both.
{ inputs, ... }:
{
  flake = {
    nixosModules.default = import ./modules/nixos.nix inputs.self;
    darwinModules.default = import ./modules/darwin.nix inputs.self;
  };
}
