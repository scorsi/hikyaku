# M2: the spool fills (2 MiB) while the stores are down: once they are back, both stores'
# /v1/status show the agent `paused: true`, masu holds the agent's `skipped-spool-full` scrape
# record of `hikyaku` and nisshi its `spool-full` event, and every spooled line is delivered.
#
# Not asserted: `hikyaku_spool_paused 1` in a stored scrape of `hikyaku` (milestone M2). ADR 0004
# pauses every scrape while the spool is full, the agent's own included, and the gauge is 0
# again before the next scrape: no stored scrape can carry it. Proposed ADR 0009 answers it.
common:
common.mkTest "spool-full-push" {
  nodes.machine = common.agent { spoolLimit = 2097152; };
  nodes.store = common.store;

  testScript = ''
    ${common.prelude}
    ${common.deliveryPrelude}
    start_all()
    store.wait_for_unit("masu.service")
    store.wait_for_unit("nisshi.service")
    machine.wait_for_unit("hikyaku.service")
    store.wait_until_succeeds(f"{NISSHI} | grep -q agent-start")
    store.succeed("systemctl stop masu.service nisshi.service")

    N = 30000
    generate("gen1", 1, N, pad=100)
    machine.wait_until_succeeds(
        "curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_spool_paused{signal=\"logs\"} 1'"
    )
    machine.wait_until_succeeds(f"{OWN} | grep -q '\"event\":\"spool-full\"'")
    store.succeed("systemctl start masu.service nisshi.service")
    store.wait_for_open_port(9472)
    store.wait_for_open_port(9473)

    # The spool stays full once delivered: the open segment (under 16 MiB, younger than an
    # hour) is never deleted (ADR 0004, spool.md), so `paused` stays true in every push.
    for port in (9472, 9473):
        try:
            store.wait_until_succeeds(
                "curl -sS --cacert ${common.certs}/ca.pem"
                f" --resolve store.example.org:{port}:127.0.0.1"
                " -H 'Authorization: Bearer test-token-reader'"
                f" https://store.example.org:{port}/v1/status"
                " | jq -e '.agents[\"node-a\"].paused == true'",
                timeout=600,
            )
        except Exception:
            print(status(port))
            diagnose_delivery()
            raise
    store.wait_until_succeeds(
        f"{MASU} | jq -s -e 'any(.service == \"hikyaku\" and .error == \"skipped-spool-full\")'", timeout=600
    )
    store.wait_until_succeeds(f"{NISSHI} | jq -s -e 'any(.event == \"spool-full\")'", timeout=600)

    # Everything spooled reaches nisshi: the spooled n are 1..k, and nisshi holds the same.
    spooled = json.loads(dump("logs", 'map(select(.service == "gen1") | .n) | sort'))
    assert 0 < len(spooled) < N and spooled == list(range(1, len(spooled) + 1)), len(spooled)
    wait_stored('"gen1"', len(spooled))
    check_stores('"gen1"', len(spooled))
    check_own_output()
  '';
}
