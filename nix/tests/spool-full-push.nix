# M2: the spool fills (2 MiB) while the stores are down. masu comes back first: nisshi still
# down, the logs backlog keeps the spool full and masu's /v1/status shows the agent `paused:
# true`, masu holds the agent's `skipped-spool-full` scrape record of `hikyaku`. Then nisshi:
# its first push says `paused: true`, nisshi holds the `spool-full` event, and the acked spool
# is emptied at once (ADR 0012): ingestion resumes and every generated line is delivered.
# ADR 0004 pauses every scrape, the agent's own included: no stored scrape carries
# `hikyaku_spool_paused 1` (ADR 0009, rejected; milestone M2 asks for the skipped record).
common:
common.mkDeliveryTest "spool-full-push" {
  nodes.machine = common.agent { spoolLimit = 2097152; };
  nodes.store = common.store;

  script = ''
    import shlex

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
    machine.wait_until_succeeds(f"{OWN} | grep >/dev/null '\"event\":\"spool-full\"'")

    # masu alone: the metrics backlog is a few scrapes, the logs backlog keeps the spool above
    # 90 %, so the agent stays paused and every push to masu says so.
    store.succeed("systemctl start masu.service")
    store.wait_for_open_port(9472)
    store.wait_until_succeeds(f"{status_cmd(9472)} | jq -e '.agents[\"node-a\"].paused == true'", timeout=600)
    store.wait_until_succeeds(
        f"{MASU} | jq -s -e 'any(.service == \"hikyaku\" and .error == \"skipped-spool-full\")'", timeout=600
    )

    # nisshi: the push of the backlog says `paused: true`; its ack empties the spool and the
    # next push, a second later at the earliest (the sender's linger), says false. A poller
    # keeps every answer of nisshi's /v1/status.
    poll = f"while :; do {status_cmd(9473)} | jq -c '.agents[\"node-a\"].paused'; sleep 0.2; done >> /tmp/nisshi-paused"
    store.succeed(f"systemd-run --unit=nisshi-paused --setenv=PATH=\"$PATH\" sh -c {shlex.quote(poll)}")
    store.succeed("systemctl start nisshi.service")
    store.wait_for_open_port(9473)
    store.wait_until_succeeds("grep -qx true /tmp/nisshi-paused", timeout=600)
    store.wait_until_succeeds(f"{NISSHI} | jq -s -e 'any(.event == \"spool-full\")'", timeout=600)

    # The acked spool holds nothing, so ingestion resumes at once: every n reaches nisshi.
    wait_stored('"gen1"', N)
    check_stores('"gen1"', N)
    machine.wait_until_succeeds(
        "curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_spool_paused{signal=\"logs\"} 0'"
    )
    for port in (9472, 9473):
        store.wait_until_succeeds(f"{status_cmd(port)} | jq -e '.agents[\"node-a\"].paused == false'", timeout=120)
    check_own_output()
  '';
}
