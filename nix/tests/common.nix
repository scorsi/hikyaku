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
            f"systemd-run --unit={unit} --property=LogRateLimitIntervalSec=0 gen {start} {count} {rate} {pad}"
        )


    def wait_generated(services, n):
        machine.wait_until_succeeds(
            f"{DUMP} --signal logs | jq -s 'map(select(.service | IN({services})) | .n) | length == {n}' | grep -qx true",
            timeout=600,
        )


    def check_exact(services, n):
        assert dump("logs", f"map(select(.service | IN({services})) | .n) | sort == [range(1; {n} + 1)]") == "true"
        assert dump("logs", "map(.stream + \":\" + (.seq | tostring)) | length == (unique | length)") == "true"
        assert dump("logs", "map(.seq) == [range(1; length + 1)]") == "true"
        assert dump("logs", "map(.event) | index(\"journal-gap\") == null") == "true"


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
      environment.etc."systemd/journald.conf.d/console.conf".text = ''
        [Journal]
        MaxLevelConsole=notice
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
