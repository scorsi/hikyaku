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
    try:
        machine.wait_until_succeeds(
            f"{DUMP} --signal metrics | jq -s 'map(select(.error == \"skipped-spool-full\")) | length > 0' | grep -qx true",
            timeout=120,
        )
    except Exception:
        sh(f"{DUMP} --signal metrics | jq -c '{{seq, ts, service, up, error}}' | tail -n 5")
        diagnose('"gen1"', N)
        raise
    # The spool-full event is in the logs stream too, from the reserve (ADR 0003, 0004).
    assert int(dump("logs", 'map(select(.event == "spool-full")) | length')) >= 1
    machine.wait_until_fails("systemctl is-active gen1.service")
    in_journal = int(machine.succeed("journalctl -o cat _SYSTEMD_UNIT=gen1.service | wc -l"))
    if in_journal != N:
        sh("journalctl -o cat MESSAGE_ID=a596d6fe7bfa4994828e72309e95d61e")
    assert in_journal == N, f"journald holds {in_journal} gen1 lines, expected {N}"
    spooled = int(dump("logs", 'map(select(.service == "gen1")) | length'))
    assert 0 < spooled < N, spooled
    # Batches are sized by the room left before 99 %: ingestion overshoots by one record, own
    # events stop at the limit; the margin covers state.json and the directories (du -b).
    size = int(machine.succeed("du -sb /var/lib/hikyaku | cut -f1"))
    assert size <= 2097152 + 64 * 1024, size

    machine.succeed("/run/current-system/specialisation/big/bin/switch-to-configuration test")
    machine.wait_for_unit("hikyaku.service")
    machine.wait_until_succeeds(f"{OWN} | grep -q '\"event\":\"spool-resumed\"'")
    wait_generated('"gen1"', N)
    check_exact('"gen1"', N)
    machine.succeed("curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_spool_paused{signal=\"logs\"} 0'")
  '';
}
