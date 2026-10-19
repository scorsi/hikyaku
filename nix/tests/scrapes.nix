# Scrape records (ADR 0003): more series than max_series → `series-limit`, a body over 2 MiB →
# `body-too-large`, a missing page → `http-404`, the agent's own /metrics → up.
common:
common.mkTest "scrapes" {
  nodes.machine =
    { pkgs, ... }:
    let
      pages = pkgs.runCommand "pages" { } ''
        mkdir $out
        for i in $(seq 300); do echo "s_$i 1" >> $out/big; done
        head -c 3145728 /dev/zero | tr '\0' x > $out/huge
      '';
    in
    {
      imports = [ (common.machine { }) ];
      systemd.services.pages = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8000 --bind 127.0.0.1 --directory ${pages}";
      };
      services.hikyaku.targets = {
        big = {
          url = "http://127.0.0.1:8000/big";
          interval = "1s";
          maxSeries = 100;
        };
        huge = {
          url = "http://127.0.0.1:8000/huge";
          interval = "1s";
        };
        absent = {
          url = "http://127.0.0.1:8000/absent";
          interval = "1s";
        };
        hikyaku = {
          url = "http://127.0.0.1:9471/metrics";
          interval = "1s";
        };
      };
    };

  testScript = ''
    ${common.prelude}
    machine.wait_for_unit("hikyaku.service")
    machine.wait_for_unit("pages.service")
    for service, error in [("big", "series-limit"), ("huge", "body-too-large"), ("absent", "http-404")]:
        machine.wait_until_succeeds(
            f"{DUMP} --signal metrics | jq -s 'map(select(.service == \"{service}\" and .up == false and .error == \"{error}\")) | length > 0' | grep -qx true"
        )
    machine.wait_until_succeeds(
        f"{DUMP} --signal metrics | jq -s 'map(select(.service == \"hikyaku\" and .up == true and (.body | contains(\"hikyaku_build_info\")))) | length > 0' | grep -qx true"
    )
    assert dump("metrics", 'map(select(.service == "big" or .service == "huge") | has("body")) | any') == "false"
    assert dump("metrics", 'map(.stream + ":" + (.seq | tostring)) | length == (unique | length)') == "true"
    machine.succeed("curl -s http://127.0.0.1:9471/metrics | grep -q 'hikyaku_scrapes_total{target=\"big\",result=\"series-limit\"} [1-9]'")
    machine.succeed("${common.validateMetrics} validate --metrics http://127.0.0.1:9471/metrics")
  '';
}
