# M2: the stores stopped for the time-scaled hour (ADR 0008: 2 minutes with 1 s scrapes, the
# record count of an hour at 30 s), generator and scrapes running: once they are back, every
# `n` is in nisshi once, the agent's 1 s scrapes cover the outage in masu, `(stream, seq)`
# unique and dense in both, fully acked closed segments deleted (ADR 0005).
common:
common.mkTest "outage" {
  nodes.machine = common.agent { };
  nodes.store = common.store;

  testScript = ''
    ${common.prelude}
    ${common.deliveryPrelude}
    import time
    from datetime import datetime, timezone

    start_all()
    store.wait_for_unit("masu.service")
    store.wait_for_unit("nisshi.service")
    machine.wait_for_unit("hikyaku.service")
    store.wait_until_succeeds(f"{NISSHI} | grep -q agent-start")
    store.wait_until_succeeds(f"{MASU} | grep -q '\"service\":\"hikyaku\"'")

    store.succeed("systemctl stop masu.service nisshi.service")
    t0 = float(machine.succeed("date +%s.%N"))
    # 500 lines/s of 200 bytes of padding for the outage: ~24 MB, so a segment closes (16 MiB).
    N = 60000
    generate("gen1", 1, N, rate=500, pad=200)
    time.sleep(120)
    machine.wait_until_fails("systemctl is-active gen1.service", timeout=300)
    t1 = float(machine.succeed("date +%s.%N"))
    assert agent_metric('hikyaku_push_requests_total{signal="logs",code="network"}') > 0
    assert agent_metric('hikyaku_push_requests_total{signal="metrics",code="network"}') > 0
    machine.succeed("ls /var/lib/hikyaku/logs/*.ndjson.zst")
    store.succeed("systemctl start masu.service nisshi.service")

    wait_stored('"gen1"', N)
    check_stores('"gen1"', N)

    def seconds(ts):
        return datetime.strptime(ts[:26], "%Y-%m-%dT%H:%M:%S.%f").replace(tzinfo=timezone.utc).timestamp()

    scrapes = sorted(seconds(t) for t in json.loads(stored(MASU, 'map(select(.service == "hikyaku" and .up) | .ts)')))
    during = [s for s in scrapes if t0 - 2 <= s <= t1 + 2]
    widest = max(b - a for a, b in zip(during, during[1:]))
    print(f"outage {t1 - t0:.0f} s, {len(during)} scrapes of hikyaku during it, widest gap {widest:.1f} s")
    assert scrapes[0] < t0 and scrapes[-1] > t1, (scrapes[0], t0, scrapes[-1], t1)
    assert widest <= 3, widest
    assert len(during) >= (t1 - t0) * 0.8, len(during)

    # Closed segments fully acked are deleted; the open segment stays.
    machine.wait_until_succeeds("! ls /var/lib/hikyaku/logs/*.ndjson.zst", timeout=120)
    assert agent_metric('hikyaku_push_requests_total{signal="logs",code="200"}') > 0
    check_own_output()
  '';
}
