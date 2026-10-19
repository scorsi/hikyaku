# The nixosTests of M1 (docs/milestones.md), one per natural group so a failure is localised;
# the orchestrator runs them as `checks.<linux-system>.<name>`.
{
  pkgs,
  lib,
  inputs,
}:
let
  common = import ./common.nix { inherit pkgs lib inputs; };
  test = file: import file common;
in
{
  basic = test ./basic.nix;
  kill-nine = test ./kill-nine.nix;
  spool-full = test ./spool-full.nix;
  journald-suppressed = test ./journald-suppressed.nix;
  spool-format = test ./spool-format.nix;
  impermanence = test ./impermanence.nix;
  scrapes = test ./scrapes.nix;
}
