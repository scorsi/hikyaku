# The spool is readable without the agent: `jq` reads open.ndjson, `zstdcat | jq` reads a
# closed segment, and both together hold every seq once (spool.md).
common:
common.mkTest "spool-format" {
  nodes.machine = common.machine { };

  testScript = ''
    ${common.prelude}
    machine.wait_for_unit("hikyaku.service")
    N = 10000  # 10000 × 2 KiB > 16 MiB: at least one segment closes
    generate("gen1", 1, N, rate=0, pad=2048)
    machine.wait_until_succeeds("ls /var/lib/hikyaku/logs/*.ndjson.zst")
    machine.wait_until_fails("systemctl is-active gen1.service")
    wait_generated('"gen1"', N)
    check_exact('"gen1"', N)

    # One command: an own event (heartbeat) between two counts would skew them.
    closed, open_, total = machine.succeed(
        "echo $(zstdcat /var/lib/hikyaku/logs/*.ndjson.zst | jq -c .seq | wc -l)"
        " $(jq -c .seq /var/lib/hikyaku/logs/open.ndjson | wc -l)"
        f" $({DUMP} --signal logs | wc -l)"
    ).split()
    print(f"closed {closed} records, open {open_} records")
    assert int(closed) > 0 and int(closed) + int(open_) == int(total), (closed, open_, total)
    seqs = machine.succeed(
        "(zstdcat /var/lib/hikyaku/logs/*.ndjson.zst; cat /var/lib/hikyaku/logs/open.ndjson) | jq -s 'map(.seq) == [range(1; length + 1)]'"
    ).strip()
    assert seqs == "true"
    # Segment names: zero-padded first-last, lexical order = seq order.
    machine.succeed("ls /var/lib/hikyaku/logs/ | grep -E '^[0-9]{20}-[0-9]{20}\\.ndjson\\.zst$'")
  '';
}
