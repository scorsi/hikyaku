# Generator at 2000 lines/s, the agent killed with SIGKILL five times at random moments: the
# spool holds n = 1..N exactly once, (stream, seq) unique, no gap (ADR 0004: exactly once into
# the spool).
common:
common.mkTest "kill-nine" {
  nodes.machine = common.machine { };

  testScript = ''
    ${common.prelude}
    import random
    import time
    machine.wait_for_unit("hikyaku.service")
    N = 60000
    generate("gen1", 1, N)
    for _ in range(5):
        time.sleep(random.uniform(1, 5))
        machine.succeed("systemctl kill -s KILL hikyaku.service")
        machine.wait_until_succeeds("systemctl is-active hikyaku.service")
    machine.wait_until_fails("systemctl is-active gen1.service")
    wait_generated('"gen1"', N)
    check_exact('"gen1"', N)
    assert len(own_events("agent-start")) == 6
  '';
}
