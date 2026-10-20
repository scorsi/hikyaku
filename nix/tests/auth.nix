# M2, ADR 0006: one agent through four configurations (specialisations, switched in place):
# an unknown token (401: counter grows with the 5-minute retry, nothing stored, data still
# spooled), another node's token (400 node-mismatch, nothing stored), a `ca_file` pinned to
# another certificate (no request reaches the stores, `error` event), then the right token:
# everything spooled meanwhile is delivered.
common:
common.mkDeliveryTest "auth" {
  nodes.machine =
    { lib, ... }:
    let
      with' = token: caFile: {
        services.hikyaku = lib.genAttrs [ "masu" "nisshi" ] (_: {
          tokenFile = lib.mkForce (common.tokenFile token);
          caFile = lib.mkForce caFile;
        });
      };
    in
    {
      imports = [ (common.agent { token = "unknown"; }) ];
      specialisation = {
        othernode.configuration = with' "nodeB" "${common.certs}/ca.pem";
        wrongca.configuration = with' "nodeA" "${common.certs}/other.pem";
        good.configuration = with' "nodeA" "${common.certs}/ca.pem";
      };
    };
  nodes.store = common.store;

  script = ''
    import time


    def switch(name):
        starts = len(own_events("agent-start"))
        # After a switch, /run/current-system is the specialisation, which has none of its own.
        machine.succeed(f"/run/booted-system/specialisation/{name}/bin/switch-to-configuration test")
        machine.wait_until_succeeds(f"test $({OWN} | grep -c '\"event\":\"agent-start\"') -gt {starts}")


    def nothing_stored():
        store.succeed("! ls /var/lib/nisshi/node-a", "! ls /var/lib/masu/archive/node-a")


    def errors(text):
        return [e for e in own_events("push-error") if text in e.get("err", "")]


    start_all()
    store.wait_for_unit("masu.service")
    store.wait_for_unit("nisshi.service")
    machine.wait_for_unit("hikyaku.service")

    with subtest("unknown token: 401, retried every 5 minutes, nothing stored"):
        for signal in ("logs", "metrics"):
            machine.wait_until_succeeds(
                f"curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_push_requests_total{{signal=\"{signal}\",code=\"401\"}} 1'"
            )
        e401 = errors("http-401")
        assert e401 and all(e["level"] == "error" and e["code"] == "401" for e in e401), e401
        assert metric(store.succeed("curl -s http://127.0.0.1:9475/metrics"), 'masu_push_requests_total{code="401"}') >= 1
        assert metric(store.succeed("curl -s http://127.0.0.1:9474/metrics"), 'nisshi_push_requests_total{code="401"}') >= 1
        nothing_stored()
        assert int(dump("logs", 'map(select(.event == "agent-start")) | length')) == 1
        assert agent_metric('hikyaku_unacked_records{signal="metrics"}') > 0
        # The stopped stream retries after 5 minutes: the counter grows, still nothing stored.
        machine.wait_until_succeeds(
            "curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_push_requests_total{signal=\"logs\",code=\"401\"} 2'",
            timeout=420,
        )
        nothing_stored()

    with subtest("another node's token: 400 node-mismatch, nothing stored"):
        switch("othernode")
        for signal in ("logs", "metrics"):
            machine.wait_until_succeeds(
                f"curl -s http://127.0.0.1:9471/metrics | grep -qx 'hikyaku_push_requests_total{{signal=\"{signal}\",code=\"400\"}} 1'"
            )
        mismatch = [e for e in own_events("push-error") if e.get("err") == "node-mismatch"]
        assert sorted(e["signal"] for e in mismatch) == ["logs", "metrics"], mismatch
        nothing_stored()

    with subtest("ca_file pinned to another certificate: no request sent, error event"):
        before = (store_pushes(9475), store_pushes(9474))
        switch("wrongca")
        machine.wait_until_succeeds(
            "curl -s http://127.0.0.1:9471/metrics | grep -q 'hikyaku_push_requests_total{signal=\"logs\",code=\"network\"} [2-9]'"
        )
        tls = errors("invalid peer certificate")
        assert tls and all(e["level"] == "error" for e in tls), tls
        time.sleep(5)
        assert (store_pushes(9475), store_pushes(9474)) == before, before
        nothing_stored()

    with subtest("the right token: everything spooled meanwhile is delivered"):
        switch("good")
        store.wait_until_succeeds(f"{NISSHI} | jq -s 'map(select(.event == \"agent-start\")) | length' | grep -qx 4")
        check_stores('"none"', 0)
        # Records of every phase, the first included: nothing was dropped while refused.
        assert stored(NISSHI, 'map(select(.event == "push-error") | .code) | unique') == '["400","401","network"]'
        check_own_output()
  '';
}
