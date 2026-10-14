# NixOS module for the PC that kontrol controls: a listener that runs the commands posted to `<topic>-commands`
# and reports on `<topic>`, an announcement after every login and resume, and the phone's HTTP
# Shortcuts import file at /etc/kontrol/shortcuts.json. The ntfy users and grants are the server's to set up.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.kontrol;
  host = config.networking.hostName;
  ntfy = lib.getExe' pkgs.ntfy-sh "ntfy";
  commandTopic = "${cfg.topic}-commands"; # the firmware's Config::parse derives the same name
  statusUrl = "${cfg.server}/${cfg.topic}";

  # ntfy runs this once per message, with the text in NTFY_MESSAGE, its unix time in NTFY_TIME and the
  # listener's NTFY_TOKEN inherited. The text is untrusted: it is only ever compared, never evaluated.
  handle = pkgs.writeShellApplication {
    name = "kontrol-handle";
    runtimeInputs = [pkgs.uutils-coreutils-noprefix pkgs.systemd];
    text = ''
      ((EPOCHSECONDS - NTFY_TIME <= 120)) || exit 0 # sent while this PC slept: TCP delivers it after resume
      # env -i: ntfy exports NTFY_TOPIC and friends to this script, and `ntfy publish` would take them as its
      # topic and message and post to the public ntfy.sh. A lost report must not cancel the command.
      report() { env -i NTFY_TOKEN="$NTFY_TOKEN" ${ntfy} publish --quiet ${statusUrl} "$1" || true; }
      case $NTFY_MESSAGE in
      ${lib.concatStrings (lib.mapAttrsToList (name: c:
        # The report goes first because suspend and poweroff take the network with them.
          if c.action == null
          then ''
            ${name}) report "${host} is already up" ;;
          ''
          else ''
            ${name})
              report "${host}: ${lib.toLower c.label}"
              ${c.action} || report "${host}: ${lib.toLower c.label} failed"
              ;;
          '')
      cfg.commands)}
      *) report "${host}: unknown command" ;;
      esac
    '';
  };

  # Announces "<host> is up" in the background, retrying for 2 minutes while the network comes up, and
  # subscribes at once. A login, a resume (resumeCommands below) and a restart after a crash each start it.
  listen = pkgs.writeShellApplication {
    name = "kontrol-listen";
    runtimeInputs = [pkgs.uutils-coreutils-noprefix pkgs.ripgrep];
    text = ''
      NTFY_TOKEN=$(rg -m1 -r '$1' '^${cfg.tokenVariable}=(.*)' ${cfg.tokenFile}) || { echo "kontrol: no ${cfg.tokenVariable}= line in ${cfg.tokenFile}" >&2; exit 1; }
      export NTFY_TOKEN
      for _ in $(seq 24); do
        ${ntfy} publish --quiet ${statusUrl} "${host} is up" && break
        sleep 5
      done &
      exec ${ntfy} subscribe ${cfg.server}/${commandTopic} ${lib.getExe handle}
    '';
  };
in {
  options.services.kontrol = {
    enable = lib.mkEnableOption "the kontrol listener, which runs commands posted to an ntfy topic";
    server = lib.mkOption {
      type = lib.types.str;
      example = "https://ntfy.example.com";
      description = "ntfy base URL, without a trailing slash.";
    };
    topic = lib.mkOption {
      type = lib.types.str;
      example = "pc";
      description = "The ntfy topic the listener and the Pico report on, and the one to subscribe to in the ntfy app. Commands go to `<topic>-commands`.";
    };
    user = lib.mkOption {
      type = lib.types.str;
      description = "User whose graphical session runs the listener; every command action runs as this user.";
    };
    tokenFile = lib.mkOption {
      type = lib.types.str;
      description = "KEY=VALUE file with the listener's ntfy token, read at start and never copied to the store.";
    };
    tokenVariable = lib.mkOption {
      type = lib.types.str;
      default = "KONTROL_TOKEN";
      description = "The key in tokenFile that holds the token.";
    };
    commands = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          label = lib.mkOption {
            type = lib.types.str;
            description = "The phone button's text, also used in the status reports.";
          };
          icon = lib.mkOption {
            type = lib.types.str;
            default = "flat_color_power";
            description = "HTTP Shortcuts icon name.";
          };
          action = lib.mkOption {
            type = lib.types.nullOr lib.types.str;
            default = null;
            description = "Shell command the listener runs, with only systemd and uutils on PATH: give other tools by store path. null for a command the Pico answers (wake).";
          };
          confirm = lib.mkOption {
            type = lib.types.bool;
            default = false;
            description = "Ask on the phone before sending.";
          };
        };
      });
      description = "The commands, by the word the phone posts. Defaults: wake, suspend, poweroff and reboot.";
    };
  };

  config = lib.mkIf cfg.enable {
    # Defaults in config, not in the option, so a host's own commands add to them instead of replacing them.
    # mkDefault per field: on the whole command, setting one field would drop the rest, its action included.
    services.kontrol.commands = lib.mapAttrs (_: lib.mapAttrs (_: lib.mkDefault)) {
      wake = {
        label = "Wake";
        icon = "flat_color_power";
      };
      suspend = {
        label = "Suspend";
        icon = "flat_color_hourglass";
        action = "systemctl suspend";
      };
      poweroff = {
        label = "Shut down";
        icon = "flat_color_stop";
        action = "systemctl poweroff";
        confirm = true;
      };
      reboot = {
        label = "Reboot";
        icon = "flat_color_loading";
        action = "systemctl reboot";
        confirm = true;
      };
    };

    environment.etc."kontrol/shortcuts.json".source = pkgs.callPackage ./shortcuts.nix {
      inherit (cfg) server commands;
      inherit commandTopic;
    };

    # partOf: restarted with each new graphical session, so commands that talk to the compositor always reach
    # the running one.
    systemd.user.services.kontrol-listener = {
      description = "Run commands from the phone (kontrol)";
      after = ["graphical-session-pre.target"];
      partOf = ["graphical-session.target"];
      wantedBy = ["graphical-session.target"];
      unitConfig.ConditionUser = cfg.user; # user units exist for every user; two listeners would run each command twice
      serviceConfig = {
        ExecStart = lib.getExe listen;
        Restart = "always";
        RestartSec = 10;
      };
    };

    # After a suspend the old connection can look open while dead; TCP keepalive takes ~5 minutes to notice.
    powerManagement.resumeCommands = ''
      ${pkgs.systemd}/bin/systemctl --machine=${cfg.user}@.host --user restart kontrol-listener.service || true
    '';
  };
}
