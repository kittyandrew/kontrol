# The firmware's app logic, built for Linux over a TAP device, against real ntfy behind Caddy with TLS, DHCP
# and DNS: only what the simulation (tests/sim) cannot reach, which also owns every timing failure. The flash
# sector and the watchdog are tests/emu's; the radio is the hardware check's.
# Run: nix build .#checks.x86_64-linux.wake
{
  build,
  lib,
  openssl,
  runCommand,
  testers,
}: let
  mac = "52:54:00:aa:bb:cc"; # the sleeping PC's; its stand-in counts only packets carrying it
  picoToken = "tk_picotestpicotestpicotest00000"; # ntfy wants tk_ and 29 more
  phoneToken = "tk_phonetestphonetestphonetest00";
  topic = "pc"; # commands go to pc-commands, as the firmware derives

  # Caddy sends a chain over 4 KiB, as a real one can be, so the firmware needs app.rs's full-record TLS_RX:
  # three RSA 4096 intermediates over an ECDSA leaf. The leaf signs the handshake, and embedded-tls offers no
  # RSA scheme without its `rsa` feature. The firmware checks no certificate (app.rs).
  certs = runCommand "kontrol-test-certs" {nativeBuildInputs = [openssl];} ''
    mkdir -p $out
    printf 'basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\n' > ca.ext
    printf 'subjectAltName=DNS:ntfy.test\n' > leaf.ext
    openssl req -x509 -newkey rsa:4096 -nodes -days 3650 -subj "/CN=test root" -addext basicConstraints=critical,CA:TRUE \
      -addext keyUsage=critical,keyCertSign -keyout root.key -out root.pem
    issue() { # name, issuer, extension file, key options
      openssl req -newkey "$4" -nodes -subj "/CN=$1" -keyout $1.key -out $1.csr
      openssl x509 -req -in $1.csr -CA $2.pem -CAkey $2.key -days 3650 -extfile $3 -out $1.pem
    }
    issue int1 root ca.ext rsa:4096
    issue int2 int1 ca.ext rsa:4096
    issue int3 int2 ca.ext rsa:4096
    openssl ecparam -name prime256v1 -out p256.pem
    issue ntfy.test int3 leaf.ext ec:p256.pem
    cat ntfy.test.pem int3.pem int2.pem int1.pem > $out/chain.pem
    cp root.pem $out/root.pem
    cp ntfy.test.key $out/leaf.key
    test "$(cat ntfy.test.pem int3.pem int2.pem int1.pem | openssl crl2pkcs7 -nocrl -certfile /dev/stdin -outform DER | wc -c)" -gt 4096
  '';

  firmware = build {
    pname = "kontrol-linux";
    target = "x86_64-unknown-linux-gnu";
    installPhase = ''
      runHook preInstall
      install -Dm555 target/x86_64-unknown-linux-gnu/release/kontrol $out/bin/kontrol
      runHook postInstall
    '';
  };
in
  testers.runNixOSTest {
    name = "kontrol";

    nodes = {
      router = {nodes, ...}: {
        networking.firewall.allowedUDPPorts = [53 67];
        services.dnsmasq = {
          enable = true;
          resolveLocalQueries = false;
          settings = {
            interface = "eth1";
            bind-interfaces = true;
            dhcp-range = "192.168.1.100,192.168.1.200";
            address = "/ntfy.test/${nodes.server.networking.primaryIPAddress}";
            no-resolv = true;
          };
        };
      };

      server = {pkgs, ...}: {
        networking.firewall.allowedTCPPorts = [443];
        environment.systemPackages = [pkgs.curl];
        services.ntfy-sh = {
          enable = true;
          settings = {
            base-url = "https://ntfy.test";
            listen-http = "127.0.0.1:2586";
            behind-proxy = true;
            auth-file = "/var/lib/ntfy-sh/user.db";
            auth-default-access = "deny-all";
            # The hashes are of "test"; the firmware and curl authenticate with the tokens.
            auth-users = [
              "pico:$2a$10$h9JFb1InI54y4Bt/LF6h0.oxwEuHmTMSyVp443/TwwxONTdXTIXJm:user"
              "phone:$2a$10$jH.5EqKZ30ZXuHwl70ALoeklcmcwASJ71n3821OSpZkb8TvoYMVzC:user"
            ];
            auth-access = [
              "pico:${topic}-commands:ro"
              "pico:${topic}:wo"
              "phone:${topic}-commands:wo"
              "phone:${topic}:ro"
            ];
            auth-tokens = ["pico:${picoToken}" "phone:${phoneToken}"];
          };
        };
        services.caddy = {
          enable = true;
          virtualHosts."ntfy.test".extraConfig = ''
            tls ${certs}/chain.pem ${certs}/leaf.key
            reverse_proxy 127.0.0.1:2586
          '';
        };
      };

      # The firmware gets its own address on the LAN: its TAP device shares a bridge with eth1.
      pico = {
        config,
        pkgs,
        ...
      }: {
        networking.interfaces.eth1.ipv4.addresses = lib.mkForce [];
        networking.interfaces.eth1.ipv6.addresses = lib.mkForce [];
        networking.interfaces.tap0 = {
          virtual = true;
          virtualType = "tap";
        };
        networking.bridges.br0.interfaces = ["eth1" "tap0"];
        networking.interfaces.br0.ipv4.addresses = [
          {
            address = config.networking.primaryIPAddress;
            prefixLength = 24;
          }
        ];
        systemd.services.kontrol = {
          wantedBy = ["multi-user.target"];
          after = ["tap0-netdev.service" "br0-netdev.service"];
          requires = ["tap0-netdev.service"];
          startLimitIntervalSec = 0; # a stall exits on purpose; it must always come back, as the board does
          environment = {
            KONTROL_TAP = "tap0";
            KONTROL_CONFIG = pkgs.writeText "kontrol-config" ''
              ssid=unused
              pass=unused
              token=${picoToken}
              server=https://ntfy.test
              topic=${topic}
              mac=${mac}
            '';
          };
          serviceConfig = {
            ExecStart = lib.getExe' firmware "kontrol";
            Restart = "always";
            RestartSec = 1;
          };
        };
      };

      pc = {pkgs, ...}: {
        networking.firewall.allowedUDPPorts = [9];
        systemd.services.wol-listener = {
          wantedBy = ["multi-user.target"];
          serviceConfig.ExecStart = pkgs.writers.writePython3 "wol-listener" {} ''
            import socket

            mac = bytes.fromhex("${lib.replaceStrings [":"] [""] mac}")
            expected = b"\xff" * 6 + mac * 16
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            s.bind(("", 9))
            while True:
                data, (addr, _) = s.recvfrom(1024)
                with open("/tmp/wol.log", "a") as log:
                    log.write(f"{'ok' if data == expected else 'bad'} {addr}\n")
          '';
        };
      };
    };

    testScript = ''
      import json
      import time

      ca = "${certs}/root.pem"


      def ntfy(args):
          return server.succeed(
              f"curl -sSf --max-time 10 --cacert {ca} --resolve ntfy.test:443:127.0.0.1"
              f" -H 'Authorization: Bearer ${phoneToken}' {args}"
          )


      def publish(message):
          return json.loads(ntfy(f"-d {message} https://ntfy.test/${topic}-commands"))["id"]


      def wakes():
          return int(pico.succeed("journalctl -u kontrol -o cat | grep -c '^wake ' || true").strip() or 0)


      def subscriptions():
          return int(pico.succeed("journalctl -u kontrol -o cat | grep -cx subscribed || true").strip() or 0)


      def subscribed(after=0):
          pico.wait_until_succeeds(f"test $(journalctl -u kontrol -o cat | grep -cx subscribed) -gt {after}", timeout=90)


      start_all()
      router.wait_for_unit("dnsmasq.service")
      server.wait_for_unit("caddy.service")
      server.wait_for_unit("ntfy-sh.service")
      pc.wait_for_unit("wol-listener.service")
      pico.wait_for_unit("kontrol.service")

      with subtest("a wake reaches the PC and is reported"):
          subscribed()
          wake = publish("wake")
          pico.wait_until_succeeds(f"journalctl -u kontrol -o cat | grep -qx 'wake {wake}'", timeout=60)
          pc.wait_until_succeeds("grep -q '^ok' /tmp/wol.log", timeout=10)
          server.wait_until_succeeds(
              f"curl -sf --cacert {ca} --resolve ntfy.test:443:127.0.0.1 -H 'Authorization: Bearer ${phoneToken}'"
              " 'https://ntfy.test/${topic}/json?poll=1' | grep -q 'Pico sent the wake packet'",
              timeout=30,
          )

      with subtest("the replay after a reconnect does not repeat a wake"):
          before, mark = wakes(), subscriptions()
          server.succeed("systemctl restart caddy")  # drops the subscription; ntfy keeps the wake for since=2m
          subscribed(mark)
          time.sleep(3)
          assert wakes() == before, "a handled wake was acted on again"

      with subtest("only well-formed packets were ever sent"):
          pc.fail("grep -q '^bad' /tmp/wol.log")
    '';
  }
