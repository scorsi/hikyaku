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
in
{
  inherit gen;
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

  mkTest =
    name: test:
    pkgs.testers.runNixOSTest (
      lib.recursiveUpdate {
        name = "hikyaku-${name}";
        node.specialArgs = { inherit inputs; };
      } test
    );
}
