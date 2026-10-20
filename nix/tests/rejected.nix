# M2, ADR 0005: a store that refuses one record (indexed 400). The real stores never refuse a
# record hikyaku builds (same rules, ADR 0003), so the refusal is staged by a test-only proxy
# in front of each real store (fakestore.py): it refuses records carrying the marker
# `refuse-me` as told, logs what it refused, and forwards everything else, so the real store
# still validates and stores the replacement. Logs and metrics: the stream continues and the
# store holds the replacement (same seq, raw_b64 = the refused bytes); a restart between the
# refusal and the resend keeps the replacement; a refused replacement stops the stream, which
# retries 5 minutes later.
common:
let
  inherit (common) pkgs certs;
  fakestore = name: listen: upstream: {
    description = "test-only refusing proxy in front of ${name}";
    wantedBy = [ "multi-user.target" ];
    after = [ "${name}.service" ];
    path = [ pkgs.zstd ];
    serviceConfig = {
      ExecStart = "${pkgs.python3}/bin/python3 ${./fakestore.py} ${toString listen} ${toString upstream} ${certs}/leaf.pem ${certs}/leaf.key /var/lib/fakestore/${name}";
      StateDirectory = "fakestore/${name}";
      Restart = "always";
    };
  };
  # A target whose exposition carries the marker, started by the script.
  markerMetrics = pkgs.writeTextDir "metrics" ''
    # HELP marker_info refuse-me
    # TYPE marker_info gauge
    marker_info 1
  '';
in
common.mkTest "rejected" {
  nodes.store = {
    imports = [ common.store ];
    systemd.services.fakestore-masu = fakestore "masu" 9572 9472;
    systemd.services.fakestore-nisshi = fakestore "nisshi" 9573 9473;
    networking.firewall.allowedTCPPorts = [
      9572
      9573
    ];
  };
  nodes.machine = {
    imports = [
      (common.agent {
        masuPort = 9572;
        nisshiPort = 9573;
      })
    ];
    services.hikyaku.targets.marker = {
      url = "http://127.0.0.1:9599/metrics";
      interval = "5s";
    };
    systemd.services.marker-target.serviceConfig.ExecStart =
      "${pkgs.python3}/bin/python3 -m http.server 9599 --bind 127.0.0.1 --directory ${markerMetrics}";
    # One schema line carrying the marker, as a service writes it.
    systemd.services.marker = {
      serviceConfig.Type = "oneshot";
      script = ''
        echo '{"v":1,"ts":"'"$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)"'","level":"info","event":"tick","tag":"refuse-me"}'
      '';
    };
  };

  testScript = ''
    ${common.prelude}
    ${common.deliveryPrelude}
    import base64
    import time

    FS = "/var/lib/fakestore"


    def mode(name, value):
        store.succeed(f"echo {value} > {FS}/{name}/mode")


    def refused(name):
        _, out = store.execute(f"cat {FS}/{name}/refused.ndjson")
        return [json.loads(line) for line in out.splitlines() if line]


    def requests(name):
        return [json.loads(line) for line in store.succeed(f"cat {FS}/{name}/requests.ndjson").splitlines()]


    def wait_refused(name, count):
        store.wait_until_succeeds(f"test $(wc -l < {FS}/{name}/refused.ndjson) -ge {count}")
        return refused(name)[count - 1]


    def replacements():
        return json.loads(machine.succeed("jq -c '.replacements.logs // {}' /var/lib/hikyaku/state.json"))


    def wait_replaced(cmd, stream, r, select):
        try:
            store.wait_until_succeeds(
                f"{cmd} | jq -s -e 'any(.stream == \"{stream}\" and .seq == {r['seq']} and .raw_b64 == \"{r['b64']}\" and {select})'",
                timeout=300,
            )
        except Exception:
            diagnose_delivery()
            raise


    start_all()
    for unit in ("masu", "nisshi", "fakestore-masu", "fakestore-nisshi"):
        store.wait_for_unit(f"{unit}.service")
    machine.wait_for_unit("hikyaku.service")
    store.wait_until_succeeds(f"{NISSHI} | grep -q agent-start")
    logs, metrics = stream_of("logs"), stream_of("metrics")

    with subtest("one refused record per signal: replaced, the stream continues"):
        mode("nisshi", "refuse-once")
        mode("masu", "refuse-once")
        machine.succeed("systemctl start marker-target.service marker.service")
        r_logs, r_metrics = wait_refused("nisshi", 1), wait_refused("masu", 1)
        wait_replaced(NISSHI, logs, r_logs,
                      '.event == "rejected" and .level == "error" and .err == "test-refused" and .service == "marker"')
        wait_replaced(MASU, metrics, r_metrics,
                      '.up == false and .error == "rejected:test-refused" and .service == "marker"')
        assert json.loads(base64.b64decode(r_logs["b64"]))["tag"] == "refuse-me"
        assert stored(NISSHI, 'map(select(.tag == "refuse-me")) | length') == "0"
        for signal in ("logs", "metrics"):
            assert agent_metric(f'hikyaku_rejected_records_total{{signal="{signal}",reason="test-refused"}}') == 1
        events = own_events("record-rejected")
        assert sorted((e["signal"], e["rejected_seq"]) for e in events) == sorted(
            [("logs", r_logs["seq"]), ("metrics", r_metrics["seq"])]), events
        generate("gen1", 1, 1000)
        wait_stored('"gen1"', 1000)

    with subtest("restart between the refusal and the resend keeps the replacement"):
        mode("nisshi", "refuse-once-then-down")
        machine.succeed("systemctl start marker.service")
        r = wait_refused("nisshi", 2)
        machine.wait_until_succeeds(f"jq -e '.replacements.logs | has(\"{r['seq']}\")' /var/lib/hikyaku/state.json")
        machine.succeed("systemctl kill -s KILL hikyaku.service")
        machine.wait_until_succeeds("systemctl is-active hikyaku.service")
        assert str(r["seq"]) in replacements(), replacements()
        mode("nisshi", "pass")
        wait_replaced(NISSHI, logs, r, '.event == "rejected"')
        assert stored(NISSHI, 'map(select(.tag == "refuse-me")) | length') == "0"
        # Refused once: after the restart the replacement, not the record, was sent.
        assert [e["rejected_seq"] for e in own_events("record-rejected")].count(r["seq"]) == 1
        assert len(refused("nisshi")) == 2
        machine.wait_until_succeeds("jq -e '.replacements.logs == {}' /var/lib/hikyaku/state.json")

    with subtest("a refused replacement stops the stream, retried after 5 minutes"):
        mode("nisshi", "refuse-replacements")
        machine.succeed("systemctl start marker.service")
        r = wait_refused("nisshi", 3)
        guard = wait_refused("nisshi", 4)
        assert guard["seq"] == r["seq"], (r, guard)
        machine.wait_until_succeeds(f"{OWN} | grep -q '\"event\":\"replacement-rejected\"'")
        time.sleep(1)  # the proxy logs a request once it has answered it
        stopped = len(requests("nisshi"))
        # The logs stream sends nothing meanwhile; metrics keep flowing.
        masu_before = len(requests("masu"))
        time.sleep(240)
        assert len(requests("nisshi")) == stopped, requests("nisshi")[stopped:]
        assert len(requests("masu")) > masu_before
        mode("nisshi", "pass")
        wait_replaced(NISSHI, logs, r, '.event == "rejected"')
        retry = requests("nisshi")[stopped]
        waited = retry["t"] - guard["t"]
        print(f"the stopped stream retried after {waited:.1f} s")
        assert 295 <= waited <= 330, waited
        assert retry["code"] == 200, retry
        check_stores('"gen1"', 1000)
        check_own_output()
  '';
}
