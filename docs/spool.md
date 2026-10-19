# Spool format

Private to hikyaku (ADR 0004); documented so that a human can read a spool without the agent.

```
<spool>/                                  default /var/lib/hikyaku
  state.json
  logs/<first_seq>-<last_seq>.ndjson.zst  closed segments, one zstd file each
  logs/open.ndjson                        open segment, plain NDJSON
  metrics/<first_seq>-<last_seq>.ndjson.zst
  metrics/open.ndjson
```

- `<first_seq>` and `<last_seq>` are zero-padded to 20 digits, so a lexical sort is the seq
  order.
- Lines are the records exactly as they will be pushed: nisshi log records in `logs/`, masu
  scrape records in `metrics/`, each with `stream` and `seq`.

`state.json`:

```json
{
  "v": 1,
  "streams": {
    "logs":    {"stream": "9b1e0c5d7a2f4e8b9c0d1e2f3a4b5c6d", "next_seq": 186423, "acked_seq": 186400, "open_bytes": 1048211},
    "metrics": {"stream": "7f3c11e0a9b84d2c8e6f5a4b3c2d1e0f", "next_seq": 5122,   "acked_seq": 5121,   "open_bytes": 3311020}
  },
  "journal_cursor": "s=…;i=…;b=…;m=…;t=…;x=…",
  "files": {"kekkai-node": {"path": "/var/log/kekkai-node.log", "inode": 1234, "offset": 99120}},
  "paused": {"logs": false, "metrics": false},
  "replacements": {"logs": {"186410": "{\"v\":1,\"ts\":\"…\",\"level\":\"error\",\"event\":\"rejected\",…}"}, "metrics": {}}
}
```

## Commit and recovery

1. Append the lines to `open.ndjson`, `fsync`.
2. Write `state.json.tmp` (new `next_seq`, `open_bytes`, source positions), `fsync`, rename,
   `fsync` the directory.

At start: truncate `open.ndjson` to `open_bytes`; anything beyond was not committed and is
read again from its source.

## Closing a segment

At 16 MiB or 1 hour: compress `open.ndjson` into `<first>-<last>.ndjson.zst.tmp`, `fsync`,
rename, then empty `open.ndjson` and commit (`open_bytes: 0`). At start, a closed segment
whose range covers the first seq of `open.ndjson` means the close completed: empty the open
segment.

## Replacements

`replacements` holds, per signal and `seq`, a record that replaces the spooled one when it is
sent (ADR 0005). An entry is removed once `acked_seq` passes its `seq`. `hikyaku spool dump`
prints the spooled record and, after it, the replacement marked with `"_replaced": true`.

## Deletion

A closed segment whose `last_seq <= acked_seq` is deleted. The open segment is never
deleted.
