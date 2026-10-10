"""Directory and sharing journeys against the actual CLI, service and the fake Silicon Accounts."""
import json
import re


def run(journey, request):
    cli, check = journey.cli, journey.check
    before_connections = cli("owner", "connection", "ls")
    catalog = cli("owner", "directory", "ls")
    community = [entry for entry in catalog if entry["source"] == "community"]
    check("Bundled community directory has attributed read-only entries", len(community) > 50 and all(
        entry["source_url"] and entry["source_revision"] and not entry["can_manage"]
        and re.fullmatch(r"[a-zA-Z0-9]{3}", entry["id"]) for entry in community
    ))
    selected = community[0]
    cli("owner", "directory", "rm", selected["id"], expected=False)
    matches = cli("owner", "directory", "ls", "--search", "github")
    check("Directory search finds provider metadata", bool(matches) and all(
        "github" in (entry["name"] + entry["description"]).lower() for entry in matches
    ))
    body = {
        "name": "fixture-catalog", "description": "A Carbon's fixture template",
        "category": "Testing", "source_url": "https://example.com/mcp-setup",
        "template": {"transport": "http", "url": journey.provider + "/mcp/public",
                     "command": None, "args": [], "auth_mode": "none"},
    }
    path = journey.directory / "directory-input.json"
    path.write_text(json.dumps(body))
    entry = cli("owner", "directory", "new", "--input", "@" + str(path))
    check("A Carbon adds a personal directory entry without creating a connection",
          entry["source"] == "personal" and entry["can_manage"] and entry["owner"]["id"] == "c:owner"
          and cli("owner", "connection", "ls") == before_connections)
    cli("silicon", "directory", "show", entry["id"], expected=False)
    check("A personal entry stays with its creator until shared, even from the Silicons it looks after")
    cli("owner", "directory", "share", entry["id"], "--account", "si:researcher")
    seen = cli("silicon", "directory", "show", entry["id"])
    check("Shared by id, the Silicon discovers the entry without management rights", not seen["can_manage"])
    cli("outsider", "directory", "show", entry["id"], expected=False)
    cli("silicon", "directory", "rm", entry["id"], expected=False)
    cli("silicon", "directory", "set", entry["id"], "--input", "@" + str(path), expected=False)
    check("Directory refuses reads by accounts it was not shared with, and edits or deletion by non-managers")

    dry = cli("silicon", "connection", "new", "catalog-dry", "--from", entry["id"], "--dry-run")
    check("Directory review performs no connection write", cli("owner", "connection", "ls") == before_connections and bool(dry))
    connection = cli("silicon", "connection", "new", "catalog-public", "--from", entry["id"])
    check("A connection made from the directory is invite-only by default, even without provider authentication",
          connection["visibility"] == "invited" and connection["owner"]["id"] == "si:researcher")
    result = cli("owner", "tool", "call", connection["id"], "echo", "--input", '{"message":"directory works"}')
    check("The custodian uses the connection its Silicon made from the directory", "directory works" in json.dumps(result))

    body["description"] = "Updated without changing existing connections"
    body["template"]["url"] = journey.provider + "/mcp/bearer"
    body["template"]["auth_mode"] = "per-user"
    path.write_text(json.dumps(body))
    revised = cli("owner", "directory", "set", entry["id"], "--input", "@" + str(path))
    unchanged = cli("silicon", "connection", "show", connection["id"])
    check("Editing a directory template leaves configured connections unchanged", revised["version"] > entry["version"] and unchanged["auth_mode"] == "none")
    personal = cli("owner", "connection", "new", "catalog-personal", "--from", entry["id"])
    check("Provider-account catalog connection defaults to invite-only", personal["visibility"] == "invited")
    cli("stranger", "connection", "show", personal["id"], expected=False)
    cli("owner", "access", "new", personal["id"], "--account", "si:researcher")
    check("Invite-only gives the invited Silicon access", cli("silicon", "connection", "show", personal["id"])["id"] == personal["id"])
    cli("owner", "connection", "set", personal["id"], "--visibility", "private")
    cli("silicon", "connection", "show", personal["id"], expected=False)
    check("Legacy owner-only reset removes invites without exposing another access mode",
          cli("owner", "access", "ls", personal["id"]) == [] and cli("owner", "connection", "show", personal["id"])["visibility"] == "invited")

    # Defaulting is a server contract, including callers that omit visibility entirely.
    headers = journey.api_session("owner")
    api_connections = []
    for mode in ("none", "shared", "per-user"):
        raw = request(journey.backend + "/api/v1/connections", {
            "name": "api-default-" + mode, "transport": "http",
            "url": journey.provider + "/mcp/public", "auth_mode": mode,
        }, headers=headers)["data"]
        api_connections.append(raw["id"])
        assert raw["visibility"] == "invited", raw
    check("Raw API omission defaults to invite-only for every authentication mode")
    refused = request(journey.backend + "/api/v1/connections", {
        "name": "api-org", "transport": "http", "url": journey.provider + "/mcp/public", "auth_mode": "none", "visibility": "org",
    }, headers=headers, expected=400)
    check("A visibility value from before 0.3.0 is refused with a precise error", refused["error"]["code"] == "visibility_removed")
    stale = request(journey.backend + "/api/v1/directory/" + entry["id"],
                    {"input": body, "version": entry["version"]}, method="PUT", headers=headers, expected=409)
    check("Concurrent directory edit returns revision conflict", stale["error"]["code"] == "revision_conflict")
    unsafe = dict(body, source_url="https://user:password@example.com/setup")
    request(journey.backend + "/api/v1/directory", unsafe, headers=headers, expected=400)
    check("Directory rejects credentials in source URLs")
    cli("owner", "directory", "rm", entry["id"])
    cli("owner", "directory", "show", entry["id"], expected=False)
    check("Removing a directory template does not remove configured connections", cli("silicon", "connection", "show", connection["id"])["id"] == connection["id"])
    for identifier in api_connections + [personal["id"]]:
        cli("owner", "connection", "rm", identifier)
    cli("silicon", "connection", "rm", connection["id"])
    check("Directory journey removes only its fixture entries and connections", cli("owner", "connection", "ls") == before_connections)
