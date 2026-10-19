# Own journal valid for nisshi, own /metrics valid for masu, an invalid schema line and a
# reserved-field line stored as `unparsed` with the expected reason, idle RSS recorded.
common:
common.mkTest "basic" {
  nodes.machine = {
    imports = [ (common.machine { }) ];
    systemd.services.badlines = {
      serviceConfig.Type = "oneshot";
      script = ''
        echo '{"v":1,"level":"info","event":"x"}'
        echo '{"v":1,"ts":"2026-10-19T08:14:03.512004117Z","level":"info","event":"x","node":"b"}'
        echo 'plain third-party line'
        echo '{"v":1,"ts":"2026-10-19T08:14:03.512004117Z","level":"info","event":"kept","n":1}'
      '';
    };
  };

  testScript = ''
    ${common.prelude}
    import time
    machine.wait_for_unit("hikyaku.service")
    machine.succeed("systemctl start badlines.service")
    machine.wait_until_succeeds(
        f"{DUMP} --signal logs | jq -s 'map(select(.service == \"badlines\")) | length == 4' | grep -qx true"
    )
    assert dump("logs", 'map(select(.service == "badlines" and .event == "unparsed") | .reason) | sort') == '["missing-field:ts","reserved-field:node"]'
    assert dump("logs", 'map(select(.service == "badlines" and .event == "line") | .msg)') == '["plain third-party line"]'
    assert dump("logs", 'map(select(.service == "badlines" and .event == "kept") | .n)') == "[1]"
    assert dump("logs", 'map(select(.service == "badlines")) | all(.source == "journald")') == "true"
    start = own_events("agent-start")
    assert start[-1]["fresh_spool"] is True, start
    assert dump("logs", 'map(select(.event == "agent-start")) | length') == "1"

    # The agent's own journal passes nisshi's validator; its /metrics passes masu's.
    machine.succeed(f"{OWN} | ${common.validateLogs} validate --logs")
    machine.succeed("${common.validateMetrics} validate --metrics http://127.0.0.1:9471/metrics")

    # Idle RSS (ADR 0002: decides on reading journald directly): in the test log.
    time.sleep(30)
    rss = machine.succeed("ps -o rss= -p $(systemctl show -p MainPID --value hikyaku.service)").strip()
    cgroup = machine.succeed("systemctl show -p MemoryCurrent --value hikyaku.service").strip()
    print(f"RSS idle: hikyaku {rss} KiB, cgroup (journalctl included) {cgroup} bytes")
    machine.succeed("ps -o pid,rss,comm --ppid $(systemctl show -p MainPID --value hikyaku.service) >&2")
  '';
}
