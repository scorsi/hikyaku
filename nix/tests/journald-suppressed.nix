# A unit flooding past its journald rate limit: `journald-suppressed` with count > 0, and the
# wording and MESSAGE_ID of the deployed systemd verified (ADR 0003): kept + suppressed = sent.
common:
common.mkTest "journald-suppressed" {
  nodes.machine = {
    imports = [ (common.machine { }) ];
    systemd.services.flood = {
      path = [ common.gen ];
      serviceConfig = {
        Type = "oneshot";
        LogRateLimitIntervalSec = "2s";
        LogRateLimitBurst = 100;
      };
      # The suppression message comes with the first line of the next interval.
      script = ''
        gen 1 5000 0 0
        sleep 3
        gen 5001 1 0 0
      '';
    };
  };

  testScript = ''
    ${common.prelude}
    import time
    machine.wait_for_unit("hikyaku.service")
    machine.succeed("systemctl start flood.service")
    machine.wait_until_succeeds(
        f"{DUMP} --signal logs | jq -s 'map(select(.event == \"journald-suppressed\" and .unit == \"flood.service\" and .count > 0)) | length > 0' | grep -qx true"
    )
    machine.wait_until_succeeds(
        "curl -s http://127.0.0.1:9471/metrics | grep -q 'hikyaku_journald_suppressed_lines_total{unit=\"flood.service\"} [1-9]'"
    )
    time.sleep(5)
    kept = int(dump("logs", 'map(select(.service == "flood" and .event == "tick")) | length'))
    suppressed = int(dump("logs", 'map(select(.event == "journald-suppressed" and .unit == "flood.service") | .count) | add'))
    print(f"flood: kept {kept}, suppressed {suppressed}")
    assert kept + suppressed == 5001, (kept, suppressed)
  '';
}
