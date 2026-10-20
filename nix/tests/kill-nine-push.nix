# M2: generator at 2000 lines/s, the agent killed with SIGKILL five times at random moments
# while it pushes: every `n` once in nisshi, `(stream, seq)` unique and dense in both stores
# (a batch cut by the kill is resent from acked_seq + 1, the stores drop what they had, ADR 0005).
common:
common.mkDeliveryTest "kill-nine-push" {
  nodes.machine = common.agent { };
  nodes.store = common.store;

  script = ''
    import random
    import time

    start_all()
    store.wait_for_unit("masu.service")
    store.wait_for_unit("nisshi.service")
    machine.wait_for_unit("hikyaku.service")
    store.wait_until_succeeds(f"{NISSHI} | grep -q agent-start")
    N = 60000
    generate("gen1", 1, N)
    for _ in range(5):
        time.sleep(random.uniform(1, 5))
        machine.succeed("systemctl kill -s KILL hikyaku.service")
        machine.wait_until_succeeds("systemctl is-active hikyaku.service")
    machine.wait_until_fails("systemctl is-active gen1.service", timeout=300)
    wait_stored('"gen1"', N)
    check_stores('"gen1"', N)
    store.wait_until_succeeds(f"{NISSHI} | jq -s 'map(select(.event == \"agent-start\")) | length' | grep -qx 6")
    check_own_output()
  '';
}
