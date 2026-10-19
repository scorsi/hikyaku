# Spool limit 2 MiB: ingestion pauses (metric, spool-full event, one skipped-spool-full scrape
# record), journald still holds the generator's lines; raising the limit and restarting resumes
# from the cursor with no loss (ADR 0004).
common:
common.mkTest "spool-full" {
  nodes.machine =
    { lib, ... }:
    {
      imports = [ (common.machine { spoolLimit = 2097152; }) ];
      specialisation.big.configuration.services.hikyaku.spoolLimit = lib.mkForce 67108864;
    };

  testScript = ''
    ${common.prelude}
    machine.wait_for_unit("hikyaku.service")
    N = 30000
    generate("gen1", 1, N, pad=100)
    machine.wait_until_succeeds(
        "curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_spool_paused{signal=\"logs\"} 1'"
    )
    machine.wait_until_succeeds(f"{OWN} | grep -q '\"event\":\"spool-full\"'")
    machine.wait_until_succeeds(
        f"{DUMP} --signal metrics | jq -s 'map(select(.error == \"skipped-spool-full\")) | length > 0' | grep -qx true"
    )
    machine.wait_until_fails("systemctl is-active gen1.service")
    assert machine.succeed("journalctl -o cat _SYSTEMD_UNIT=gen1.service | wc -l").strip() == str(N)
    spooled = int(dump("logs", 'map(select(.service == "gen1")) | length'))
    assert 0 < spooled < N, spooled
    # The pause is evaluated after each commit: the overshoot is at most one journald batch
    # (1000 records of ~300 bytes here).
    size = int(machine.succeed("du -sb /var/lib/hikyaku | cut -f1"))
    assert size <= 2097152 + 1000 * 400, size

    machine.succeed("/run/current-system/specialisation/big/bin/switch-to-configuration test")
    machine.wait_for_unit("hikyaku.service")
    machine.wait_until_succeeds(f"{OWN} | grep -q '\"event\":\"spool-resumed\"'")
    wait_generated('"gen1"', N)
    check_exact('"gen1"', N)
    machine.succeed("curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_spool_paused{signal=\"logs\"} 0'")
  '';
}
