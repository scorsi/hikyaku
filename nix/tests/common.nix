# Shared by the M1 nixosTests: the agent's module on the VM, a generator writing schema lines
# with a counter `n` (ADR 0008), the stores' validators from the flake inputs, and the jq
# assertions ("no loss, no duplicate" = the set of `n` is exactly 1..N, `(stream, seq)` unique).
{
  pkgs,
  lib,
  inputs,
}:
let
  system = pkgs.stdenv.hostPlatform.system;
  # gen <start> <count> <rate per second, 0 = unbounded> <pad bytes, 0 = none>
  gen = pkgs.writeScriptBin "gen" ''
    #!${pkgs.python3}/bin/python3
    import json, sys, time
    start, count, rate, pad = (int(a) for a in sys.argv[1:5])
    for i in range(count):
        t = time.time()
        ts = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(t)) + ".%09dZ" % int((t % 1) * 1e9)
        line = {"v": 1, "ts": ts, "level": "info", "event": "tick", "n": start + i}
        if pad:
            line["pad"] = "x" * pad
        sys.stdout.write(json.dumps(line, separators=(",", ":")) + "\n")
        if rate and (i + 1) % max(rate // 20, 1) == 0:
            sys.stdout.flush()
            time.sleep(0.05)
    sys.stdout.flush()
  '';
  # M2: a test CA and a leaf for the stores' TLS name, and another self-signed certificate a
  # `ca_file` can be pinned to by mistake. Test-only values, no deployment data.
  certs = pkgs.runCommand "hikyaku-test-certs" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out && cd $out
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj /CN=hikyaku-test-ca \
      -keyout ca.key -out ca.pem
    openssl req -newkey rsa:2048 -nodes -subj /CN=store.example.org -keyout leaf.key -out leaf.csr
    openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 3650 \
      -extfile <(printf 'subjectAltName=DNS:store.example.org,IP:127.0.0.1') -out leaf.pem
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj /CN=other \
      -keyout other.key -out other.pem
  '';
  tokens = {
    nodeA = "test-token-node-a";
    nodeB = "test-token-node-b";
    reader = "test-token-reader";
    unknown = "test-token-unknown";
  };
  tokenFile = name: "${pkgs.writeText "token-${name}" tokens.${name}}";
  sha256 = builtins.hashString "sha256";
  mib = 1024 * 1024;
in
rec {
  inherit
    pkgs
    gen
    certs
    tokenFile
    ;
  validateLogs = inputs.nisshi.lib.validateLogs.${system};
  validateMetrics = inputs.masu.lib.validateMetrics.${system};

  # Python helpers for the test scripts.
  prelude = ''
    import json

    DUMP = "hikyaku spool dump --spool /var/lib/hikyaku"
    OWN = "journalctl -o cat _SYSTEMD_UNIT=hikyaku.service"


    def dump(signal, jq):
        return machine.succeed(f"{DUMP} --signal {signal} | jq -s -c '{jq}'").strip()


    def generate(unit, start, count, rate=2000, pad=0):
        machine.succeed(
            f"systemd-run --unit={unit} gen {start} {count} {rate} {pad}"
        )


    def sh(cmd):
        # Diagnostics only: never fails, prints what it got.
        status, out = machine.execute(cmd)
        print(f"$ {cmd}  [exit {status}]\n{out.rstrip()}")
        return out


    def ranges(values):
        out = []
        for x in sorted(values):
            if out and x == out[-1][1] + 1:
                out[-1][1] = x
            else:
                out.append([x, x])
        return out


    def diagnose(services, n):
        """On failure: counts, missing and duplicate `n` ranges with their spool neighbourhood,
        journald's own counts, the agent's events, state, segments, journal files, metrics."""
        print("=== diagnose ===")
        ns = json.loads(machine.succeed(f"{DUMP} --signal logs | jq -s -c 'map(select(.service | IN({services})) | .n)'"))
        counts = {}
        for x in ns:
            counts[x] = counts.get(x, 0) + 1
        missing = ranges(set(range(1, n + 1)) - counts.keys())
        dups = ranges([x for x, c in counts.items() if c > 1])
        print(f"spooled {len(ns)}, distinct {len(counts)}, expected {n}")
        print(f"missing {sum(b - a + 1 for a, b in missing)} in {len(missing)} ranges: {missing[:20]}")
        print(f"duplicated {sum(b - a + 1 for a, b in dups)} in {len(dups)} ranges: {dups[:20]}")
        for a, _ in (missing[:3] + dups[:3]):
            sh(f"{DUMP} --signal logs | jq -c 'select(.n == {a - 1} or .n == {a})"
               " | {seq, ts, service, n}'")
        brief = "{seq, ts, service, event, n, from_ts, to_ts, fresh_spool, err}"
        sh(f"{DUMP} --signal logs | jq -c 'select(.service == \"hikyaku\" and .event != \"heartbeat\") | {brief}'")
        sh(f"{DUMP} --signal logs | jq -s -c 'map(.seq) as $s | [length, $s[0], $s[-1]]'")
        for unit in json.loads(f"[{services}]"):
            sh(f"journalctl -o cat _SYSTEMD_UNIT={unit}.service | wc -l")
            sh(f"journalctl -o cat _SYSTEMD_UNIT={unit}.service | jq -s -c '[length, (map(.n) | unique | length), .[0].n, .[-1].n]'")
        sh("journalctl -o cat MESSAGE_ID=a596d6fe7bfa4994828e72309e95d61e")
        sh(f"{OWN} | grep -v '\"event\":\"heartbeat\"' | tail -n 40")
        sh("systemctl show hikyaku.service -p NRestarts -p ActiveState -p SubState -p Result -p MemoryPeak")
        sh("cat /var/lib/hikyaku/state.json")
        sh("ls -la /var/lib/hikyaku/logs /var/lib/hikyaku/metrics; du -sb /var/lib/hikyaku")
        sh('c=$(jq -r .journal_cursor /var/lib/hikyaku/state.json); echo "$c";'
           ' journalctl -o json --cursor="$c" --lines=3 | jq -c "{__CURSOR, _SYSTEMD_UNIT, MESSAGE}"')
        for cmd in ["journalctl --list-boots", "journalctl --disk-usage", "ls -la /var/log/journal/*", "ls -la /run/log/journal/*"]:
            sh(cmd)
        sh("journalctl -b -1 -n 3 -o short-iso-precise")
        sh("curl -s http://127.0.0.1:9471/metrics | grep -v '^#'")
        print("=== end diagnose ===")


    def wait_generated(services, n):
        try:
            machine.wait_until_succeeds(
                f"{DUMP} --signal logs | jq -s 'map(select(.service | IN({services})) | .n) | length == {n}' | grep -qx true",
                timeout=600,
            )
        except Exception:
            diagnose(services, n)
            raise


    def check_exact(services, n):
        checks = {
            "n exactly 1..N": dump("logs", f"map(select(.service | IN({services})) | .n) | sort == [range(1; {n} + 1)]"),
            "(stream, seq) unique": dump("logs", "map(.stream + \":\" + (.seq | tostring)) | length == (unique | length)"),
            "seq dense from 1": dump("logs", "map(.seq) == [range(1; length + 1)]"),
            "no journal-gap": dump("logs", "map(.event) | index(\"journal-gap\") == null"),
        }
        failed = [k for k, v in checks.items() if v != "true"]
        if failed:
            diagnose(services, n)
        assert not failed, failed


    def own_events(event):
        return json.loads(machine.succeed(f"{OWN} | jq -s -c 'map(select(.event == \"{event}\"))'"))
  '';

  # A VM with the agent, no destinations (M1), and the tools the scripts use.
  machine =
    {
      spoolLimit ? 1073741824,
    }:
    {
      imports = [ inputs.self.nixosModules.default ];
      environment.systemPackages = [
        gen
        pkgs.jq
        pkgs.zstd
        pkgs.curl
      ];
      services.hikyaku = {
        enable = true;
        inherit spoolLimit;
      };
      # The test instrumentation forwards every journal entry to the 115200-baud serial
      # console, synchronously: journald then stores ~100 lines/s and lags the generators by
      # minutes. A drop-in overrides journald.conf; failures (notice and above) stay visible.
      # No default rate limit: a generator must reach the journal whole (exactly-once checks).
      # A unit's LogRateLimitIntervalSec=0 means "journald's default", not "off". With the
      # global limit off, a unit's own non-zero limit did not apply in the VM: journald-suppressed
      # restores the defaults in its node.
      environment.etc."systemd/journald.conf.d/console.conf".text = ''
        [Journal]
        MaxLevelConsole=notice
        RateLimitIntervalSec=0
      '';
    };

  # M2: the store VM, masu and nisshi through their own modules (TLS through LoadCredential,
  # tokens as hashes, metrics listeners set explicitly: their default is off). masu 9472, nisshi
  # 9473 on every address; their /metrics on loopback 9475 and 9474.
  store = {
    imports = [
      inputs.masu.nixosModules.default
      inputs.nisshi.nixosModules.default
    ];
    services =
      let
        common = {
          enable = true;
          tls = {
            certFile = "${certs}/leaf.pem";
            keyFile = "${certs}/leaf.key";
          };
          tokens = {
            ${sha256 tokens.nodeA} = "node:node-a";
            ${sha256 tokens.nodeB} = "node:node-b";
            ${sha256 tokens.reader} = "reader";
          };
          # The VM disk is small: the stores' 2 GiB default reserve would answer 507 forever.
          diskReserve = 8 * mib;
        };
      in
      {
        masu = common // {
          listen = [ "0.0.0.0:9472" ];
          metrics.listen = "127.0.0.1:9475";
        };
        nisshi = common // {
          listen = [ "0.0.0.0:9473" ];
          metrics.listen = "127.0.0.1:9474";
        };
      };
    # Test node only: the scenarios stop and start the stores more than StartLimitBurst allows.
    systemd.services.masu.startLimitIntervalSec = 0;
    systemd.services.nisshi.startLimitIntervalSec = 0;
    networking.firewall.allowedTCPPorts = [
      9472
      9473
    ];
    virtualisation.memorySize = 2048;
    virtualisation.diskSize = 4096;
    environment.systemPackages = [
      pkgs.zstd
      pkgs.jq
      pkgs.curl
    ];
  };

  # M2: the agent VM of `machine` with both destinations on the store VM: an IP literal and
  # `server_name` (ADR 0005, 0006), the pinned test CA, the node's token; the agent scrapes its
  # own /metrics every second (ADR 0008's time scaling).
  agent =
    {
      spoolLimit ? 1073741824,
      token ? "nodeA",
      masuPort ? 9472,
      nisshiPort ? 9473,
    }:
    { nodes, ... }:
    let
      destination = port: {
        url = "https://${nodes.store.networking.primaryIPAddress}:${toString port}";
        serverName = "store.example.org";
        caFile = "${certs}/ca.pem";
        tokenFile = tokenFile token;
      };
    in
    {
      imports = [ (machine { inherit spoolLimit; }) ];
      services.hikyaku = {
        # The stores map the tokens to `node:node-a` / `node:node-b`; the default would be the
        # VM's host name `machine` (every push 400 node-mismatch).
        node = "node-a";
        targets.hikyaku = {
          url = "http://127.0.0.1:9471/metrics";
          interval = "1s";
        };
        masu = destination masuPort;
        nisshi = destination nisshiPort;
      };
      # Test node only: kill -9 loops and configuration switches restart it often.
      systemd.services.hikyaku.startLimitIntervalSec = 0;
      # Test node only: the agent logs at info and the console is notice-level, so its lines
      # (push errors, panics) are copied to the console live: a hung or interrupted run shows
      # them too. Written to the console directly, not through journald.
      systemd.services.hikyaku-console = {
        wantedBy = [ "multi-user.target" ];
        before = [ "hikyaku.service" ];
        path = [
          pkgs.systemd
          pkgs.gnugrep
          pkgs.gnused
        ];
        script = ''
          journalctl -b -f -n all -o cat _SYSTEMD_UNIT=hikyaku.service \
            | grep --line-buffered -v '"event":"heartbeat"' | sed -u 's/^/hikyaku: /'
        '';
        serviceConfig = {
          StandardOutput = "tty";
          TTYPath = "/dev/console";
          Restart = "always";
        };
      };
    };

  # Python helpers of the M2 scripts: `machine` is the agent VM, `store` the store VM. `URLS`
  # are the agent's configured destinations (the diagnostics reach them as the agent does).
  deliveryPrelude = nodes: ''
    import urllib.parse

    NISSHI = "zstdcat /var/lib/nisshi/node-a/*.ndjson.zst 2>/dev/null"
    MASU = "zstdcat /var/lib/masu/archive/node-a/*.ndjson.zst 2>/dev/null"
    URLS = {"logs": "${nodes.machine.services.hikyaku.nisshi.url}", "metrics": "${nodes.machine.services.hikyaku.masu.url}"}
    ZEROS = "0" * 32
    UNIQUE = 'map(.stream + ":" + (.seq | tostring)) | length == (unique | length)'
    # The agent's streams hold seq 1..max exactly (nisshi's own seq-gap records excluded).
    DENSE = f'map(select(.stream != "{ZEROS}")) | group_by(.stream) | all(map(.seq) | sort == [range(1; length + 1)])'


    def stored(cmd, jq):
        return store.succeed(f"{cmd} | jq -s -c '{jq}'").strip()


    def status_cmd(port, host="127.0.0.1"):
        return ("curl -sS --max-time 20 --cacert ${certs}/ca.pem"
                f" --resolve store.example.org:{port}:{host}"
                " -H 'Authorization: Bearer ${tokens.reader}'"
                f" https://store.example.org:{port}/v1/status")


    def metric(text, sample):
        for line in text.splitlines():
            if line.startswith(sample + " "):
                return float(line.split()[-1])
        return 0.0


    def agent_metric(sample):
        return metric(machine.succeed("curl -s http://127.0.0.1:9471/metrics"), sample)


    def store_pushes(port):
        # Every push request a store answered, whatever the code.
        text = store.succeed(f"curl -s http://127.0.0.1:{port}/metrics")
        return sum(float(l.split()[-1]) for l in text.splitlines() if "_push_requests_total{" in l)


    def stream_of(signal):
        return machine.succeed(f"jq -r .streams.{signal}.stream /var/lib/hikyaku/state.json").strip()


    def run_on(vm, cmd):
        # Diagnostics only: never fails, prints what it got.
        try:
            status_, out = vm.execute(cmd, timeout=60)
            print(f"{vm.name}$ {cmd}  [exit {status_}]\n{out.rstrip()}")
        except Exception as e:
            print(f"{vm.name}$ {cmd}  [failed: {e}]")


    def diagnose_delivery():
        """Run by every M2 script on failure (mkDeliveryTest): the agent's view (its journal with
        push errors and panics, counters, state, spool, network), each store's (journal, push
        counters by code, /v1/status), and the agent VM's own TLS request to each destination."""
        print("=== diagnose delivery ===")
        run_on(machine, f"{OWN} | grep -v '\"event\":\"heartbeat\"' | tail -n 80")
        run_on(machine, f"{OWN} | jq -R -r 'fromjson? | select(.event == \"push-error\") | [.signal, .code, .err] | @tsv'"
                        " | sort | uniq -c")
        run_on(machine, "journalctl -u hikyaku.service -n 30 -o short-monotonic | grep -v '\"event\":\"heartbeat\"'")
        run_on(machine, "systemctl show hikyaku.service -p ActiveState -p SubState -p NRestarts -p ExecMainStatus -p MemoryPeak")
        run_on(machine, "curl -s http://127.0.0.1:9471/metrics | grep -v '^#'")
        run_on(machine, "head -c 4000 /var/lib/hikyaku/state.json")
        run_on(machine, "ls -la /var/lib/hikyaku /var/lib/hikyaku/logs /var/lib/hikyaku/metrics")
        run_on(machine, "ip -br addr; ip route; ss -tn")
        for signal, url in URLS.items():
            u = urllib.parse.urlsplit(url)
            run_on(machine, f"{status_cmd(u.port, u.hostname)} -v 2>&1 | tail -n 40")
        run_on(store, "journalctl -u masu -u nisshi -u 'fakestore-*' -n 80 -o cat")
        run_on(store, "curl -s http://127.0.0.1:9474/metrics http://127.0.0.1:9475/metrics | grep -v '^#'")
        for port in (9472, 9473):
            run_on(store, status_cmd(port))
        run_on(store, "ss -tln; ls -laR /var/lib/nisshi /var/lib/masu | head -n 60")
        run_on(store, f"{NISSHI} | jq -s -c 'group_by(.service) | map([.[0].service, length])'")
        run_on(store, f"{MASU} | jq -s -c 'group_by(.service) | map([.[0].service, length])'")
        print("=== end diagnose delivery ===")


    def wait_stored(services, n, timeout=900):
        store.wait_until_succeeds(
            f"{NISSHI} | jq -s 'map(select(.service | IN({services})) | .n) | length >= {n}' | grep -qx true",
            timeout=timeout,
        )


    def check_stores(services, n):
        checks = {
            "nisshi: n exactly 1..N": stored(NISSHI, f"map(select(.service | IN({services})) | .n) | sort == [range(1; {n} + 1)]"),
            "nisshi: (stream, seq) unique": stored(NISSHI, UNIQUE),
            "nisshi: agent stream dense": stored(NISSHI, DENSE),
            "nisshi: no journal-gap": stored(NISSHI, 'map(.event) | index("journal-gap") == null'),
            "masu: (stream, seq) unique": stored(MASU, UNIQUE),
            "masu: agent stream dense": stored(MASU, DENSE),
        }
        failed = [k for k, v in checks.items() if v != "true"]
        assert not failed, failed


    def check_own_output():
        # The agent's journal is nisshi's schema, its /metrics masu's convention (TLS and
        # push errors included).
        machine.succeed(f"{OWN} | ${inputs.nisshi.lib.validateLogs.${system}} validate --logs")
        machine.succeed("${
          inputs.masu.lib.validateMetrics.${system}
        } validate --metrics http://127.0.0.1:9471/metrics")
  '';

  # An M2 test: `script` runs after both preludes, inside a try whose failure branch prints
  # diagnose_delivery() before the traceback (a timed-out wait included).
  mkDeliveryTest =
    name:
    { script, ... }@test:
    mkTest name (
      builtins.removeAttrs test [ "script" ]
      // {
        testScript =
          { nodes, ... }:
          ''
            ${prelude}
            ${deliveryPrelude nodes}
            try:
            ${lib.concatMapStringsSep "\n" (l: if l == "" then l else "    " + l) (lib.splitString "\n" script)}
            except BaseException:
                try:
                    diagnose_delivery()
                except BaseException as e:
                    print(f"diagnose_delivery failed: {e!r}")
                raise
          '';
      }
    );

  mkTest =
    name: test:
    pkgs.testers.runNixOSTest (
      lib.recursiveUpdate {
        name = "hikyaku-${name}";
        node.specialArgs = { inherit inputs; };
      } test
    );
}
