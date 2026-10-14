# The phone's import file for HTTP Shortcuts (Waboodoo, format of version 4.8.2): one button per command, each
# a POST of the command word with the token from a secret app variable. Ids are UUIDv4-shaped hashes of fixed
# names, so a re-import updates the same buttons; the token variable is left empty and excluded from export.
{
  formats,
  lib,
  server,
  commandTopic,
  commands,
}: let
  uuid = name: let
    h = builtins.hashString "sha256" "kontrol/${name}";
    s = builtins.substring;
  in "${s 0 8 h}-${s 8 4 h}-4${s 13 3 h}-8${s 17 3 h}-${s 20 12 h}";
  tokenVar = uuid "token";
in
  (formats.json {}).generate "kontrol-shortcuts.json" {
    version = 92;
    compatibilityVersion = 90;
    variables = [
      {
        id = tokenVar;
        key = "kontrol_token";
        type = "constant";
        isSecret = true;
        isExcludeValueFromExport = true;
      }
    ];
    categories = [
      {
        id = uuid "category";
        name = commandTopic;
        shortcuts = lib.mapAttrsToList (name: c:
          {
            id = uuid name;
            name = c.label;
            iconName = c.icon;
            method = "POST";
            url = "${server}/${commandTopic}";
            requestBodyType = "custom_text";
            contentType = "text/plain";
            bodyContent = name;
            headers = [
              {
                key = "Authorization";
                value = "Bearer {{${tokenVar}}}"; # the app fills in {{<variable id>}} when it sends
              }
            ];
            responseHandling = {
              uiType = "toast";
              successOutput = "message";
              successMessage = "${lib.toLower c.label} sent";
              failureOutput = "simple";
            };
          }
          // lib.optionalAttrs c.confirm {confirmation = "simple";})
        commands;
      }
    ];
  }
