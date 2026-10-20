# tmpfs root, /var/lib/hikyaku and /var/log on persistent volumes. Reboot: same stream ids, no
# loss, no duplicate across boots. Wipe the spool and reboot: new stream ids, the agent starts
# from its declared configuration alone (ADR 0007).
common:
common.mkTest "impermanence" {
  nodes.machine = {
    imports = [ (common.machine { }) ];
    virtualisation.diskImage = null;
    # A persistent node keeps its machine-id: journalctl only reads the journal directory of
    # the current one, so a new id per boot hides the previous boot (a real journal-gap).
    environment.etc."machine-id".text = "6f2a1c9e4b7d4e0f8a3b5c6d7e8f9a0b\n";
    virtualisation.emptyDiskImages = [
      {
        size = 1024;
        driveConfig.deviceExtraOpts.serial = "spool";
      }
      {
        size = 1024;
        driveConfig.deviceExtraOpts.serial = "log";
      }
    ];
    virtualisation.fileSystems."/var/lib/hikyaku" = {
      device = "/dev/disk/by-id/virtio-spool";
      fsType = "ext4";
      autoFormat = true;
    };
    virtualisation.fileSystems."/var/log" = {
      device = "/dev/disk/by-id/virtio-log";
      fsType = "ext4";
      autoFormat = true;
    };
    systemd.services.hikyaku.unitConfig.RequiresMountsFor = [
      "/var/lib/hikyaku"
      "/var/log"
    ];
  };

  testScript = ''
    ${common.prelude}


    def streams():
        state = json.loads(machine.succeed("cat /var/lib/hikyaku/state.json"))
        return {k: v["stream"] for k, v in state["streams"].items()}


    machine.wait_for_unit("hikyaku.service")
    N = 20000
    generate("gen1", 1, N)
    machine.wait_until_fails("systemctl is-active gen1.service")
    wait_generated('"gen1"', N)
    first = streams()

    machine.shutdown()
    machine.start()
    machine.wait_for_unit("hikyaku.service")
    assert streams() == first, (streams(), first)
    generate("gen2", N + 1, N)
    machine.wait_until_fails("systemctl is-active gen2.service")
    wait_generated('"gen1", "gen2"', 2 * N)
    check_exact('"gen1", "gen2"', 2 * N)
    assert own_events("agent-start")[-1]["fresh_spool"] is False

    machine.succeed("systemctl stop hikyaku.service && rm -rf /var/lib/hikyaku/*")
    machine.shutdown()
    machine.start()
    machine.wait_for_unit("hikyaku.service")
    wiped = streams()
    assert wiped["logs"] != first["logs"] and wiped["metrics"] != first["metrics"], (wiped, first)
    assert own_events("agent-start")[-1]["fresh_spool"] is True
    machine.wait_until_succeeds(f"{DUMP} --signal logs | jq -s 'map(select(.event == \"agent-start\")) | length == 1' | grep -qx true")
  '';
}
