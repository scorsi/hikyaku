# The nixosTests of M1 and M2 (docs/milestones.md), one per natural group so a failure is
# localised; the orchestrator runs them as `checks.<linux-system>.<name>`. M2 tests run two
# VMs: `machine` (the agent) and `store` (masu and nisshi from the flake inputs).
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
  # M2
  outage = test ./outage.nix;
  kill-nine-push = test ./kill-nine-push.nix;
  spool-full-push = test ./spool-full-push.nix;
  auth = test ./auth.nix;
  rejected = test ./rejected.nix;
}
