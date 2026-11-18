"use client";

import { useEffect, useId, useState, type KeyboardEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/client/api";
import { AccountChip } from "@/components/foundation/account/account-chip";
import { Cluster, Section, Stack } from "@/components/foundation/layout/layout";
import { Button, ErrorBox, Input } from "./ui";
import { message, type AccountRef } from "./lib/api";

type Person = AccountRef & { trust?: number | null; photo_id?: string | null };
type Directory = { connected: boolean; items: Person[] };
const path = "/api/v1";
const directoryKey = ["mcport-people"] as const;
function usePeople() {
  return useQuery({
    queryKey: directoryKey,
    queryFn: async ({ signal }) =>
      (await api.get<{ data: Directory }>(path + "/people", { signal })).data,
    staleTime: 30_000,
  });
}
function matches(query: string, value: string) {
  const haystack = value.toLowerCase();
  let index = 0;
  for (const letter of query.toLowerCase().replace(/\s/g, "")) {
    index = haystack.indexOf(letter, index);
    if (index === -1) return false;
    index++;
  }
  return true;
}
function Peer({ person }: { person: Person }) {
  const photo = person.pfp_url?.startsWith("/api/v1/contacts/photos/")
    ? "/api" + person.pfp_url
    : person.pfp_url;
  return (
    <AccountChip
      account={{
        ...person,
        id: person.id || person.uuid,
        kind: person.kind === "silicon" ? "silicon" : "carbon",
        pfp_url: photo,
      }}
      note={person.trust ? "Trust " + person.trust : undefined}
    />
  );
}
/** The private address book is applied only while composing the current viewer's UI. */
export function PersonLabel({ account }: { account: AccountRef }) {
  const directory = usePeople();
  if (account.kind === "team" || account.kind === "organization")
    return <strong>{account.display_name || account.id}</strong>;
  return (
    <Peer
      person={
        directory.data?.items.find((person) => person.uuid === account.uuid) ??
        account
      }
    />
  );
}

type PickerProps = {
  label: string;
  value: string;
  onChange: (id: string) => void;
  kind?: "silicon";
  placeholder?: string;
  required?: boolean;
  onSelect?: (person: Person) => void;
};
export function PersonPicker({
  label,
  value,
  onChange,
  kind,
  placeholder,
  required,
  onSelect,
}: PickerProps) {
  const directory = usePeople();
  const id = useId();
  const [focused, setFocused] = useState(false);
  const [active, setActive] = useState(-1);
  const [settled, setSettled] = useState("");
  const query = value.trim();
  useEffect(() => {
    const timer = setTimeout(() => setSettled(query), 250);
    return () => clearTimeout(timer);
  }, [query]);
  const known = directory.data?.items ?? [];
  const exactKnown = known.find(
    (person) =>
      person.uuid === query || person.id.toLowerCase() === query.toLowerCase(),
  );
  const fullId =
    /^(c|si):[a-z0-9_-]{3,30}(\.[a-z0-9_-]{3,30})?$/i.test(settled) ||
    /^[a-f0-9-]{36}$/i.test(settled);
  const exact = useQuery({
    queryKey: ["mcport-person", settled],
    enabled: fullId && settled === query && !exactKnown,
    retry: false,
    staleTime: 30_000,
    queryFn: async ({ signal }) =>
      (
        await api.get<{ data: Person }>(path + "/people/resolve", {
          query: { q: settled },
          signal,
        })
      ).data,
  });
  const selected = exactKnown ?? (settled === query ? exact.data : undefined);
  const preview =
    selected && (!kind || selected.kind === kind) ? selected : undefined;
  const items = known
    .filter(
      (p) =>
        (!kind || kind === p.kind) &&
        (matches(query, p.display_name) ||
          matches(query, p.id) ||
          p.uuid === query),
    )
    .slice(0, 8);
  if (preview && !items.some((p) => p.uuid === preview.uuid))
    items.unshift(preview);
  const expanded = focused && items.length > 0;
  function choose(person: Person) {
    onChange(person.id || person.uuid);
    onSelect?.(person);
    setActive(-1);
    setFocused(false);
  }
  function keyboard(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Escape" && focused) {
      event.preventDefault();
      event.stopPropagation();
      setFocused(false);
      return;
    }
    if (!items.length) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setFocused(true);
      setActive(
        event.key === "ArrowDown"
          ? (active + 1) % items.length
          : active <= 0
            ? items.length - 1
            : active - 1,
      );
    } else if (event.key === "Enter" && expanded && (active >= 0 || !preview)) {
      event.preventDefault();
      choose(items[Math.max(0, Math.min(active, items.length - 1))]!);
    }
  }
  return (
    <div className="person-picker">
      <Input
        label={label}
        value={value}
        required={required}
        role="combobox"
        autoComplete="off"
        spellCheck={false}
        aria-autocomplete="list"
        aria-expanded={expanded}
        aria-controls={expanded ? id : undefined}
        aria-activedescendant={
          expanded && active >= 0 ? id + "-" + active : undefined
        }
        placeholder={
          placeholder ?? (kind ? "si:scout" : "Name, c:ada or si:scout")
        }
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onKeyDown={keyboard}
        onChange={(event) => {
          onChange(event.target.value);
          setActive(-1);
          setFocused(true);
        }}
      />
      {expanded && (
        <ul
          id={id}
          role="listbox"
          aria-label={label + " suggestions"}
          className="person-options"
          data-sq="surface"
        >
          {items.map((person, index) => (
            <li
              key={person.uuid}
              id={id + "-" + index}
              role="option"
              aria-selected={index === active}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(person)}
            >
              <Peer person={person} />
            </li>
          ))}
        </ul>
      )}
      {preview && (
        <div className="person-preview">
          <Peer person={preview} />
        </div>
      )}
      {focused && directory.error && (
        <ErrorBox error={message(directory.error)} />
      )}
    </div>
  );
}

export function ContactSettings() {
  const directory = usePeople();
  const cache = useQueryClient();
  const [target, setTarget] = useState("");
  const [name, setName] = useState("");
  const [trust, setTrust] = useState("1");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const saved = directory.data?.items.filter((p) => !!p.trust) ?? [];
  async function change(action: () => Promise<unknown>) {
    setError("");
    setBusy(true);
    try {
      await action();
      await cache.invalidateQueries({ queryKey: directoryKey });
      await cache.invalidateQueries({ queryKey: ["mcport-person"] });
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  function edit(person: Person) {
    setTarget(person.id || person.uuid);
    setName(person.display_name || person.id);
    setTrust(String(person.trust ?? 1));
  }
  function connect(enabled: boolean) {
    return change(() => api.put(path + "/contacts/connection", { enabled }));
  }
  async function save() {
    await change(async () => {
      await api.post(path + "/contacts", {
        id: target,
        name,
        trust: Number(trust),
      });
      setTarget("");
      setName("");
      setTrust("1");
    });
  }
  return (
    <Section
      title="Contacts"
      description="Use your Silicon Contacts names, photos and trust levels in McPort."
    >
      <Stack>
        {(error || directory.error) && (
          <ErrorBox error={error || message(directory.error)} />
        )}
        {directory.data &&
          (directory.data.connected ? (
            <>
              <form
                onSubmit={(event) => {
                  event.preventDefault();
                  void save();
                }}
              >
                <Stack>
                  <PersonPicker
                    label="Contact account"
                    value={target}
                    onChange={setTarget}
                    onSelect={edit}
                    required
                  />
                  <Cluster>
                    <Input
                      label="Saved name"
                      value={name}
                      required
                      maxLength={200}
                      onChange={(e) => setName(e.target.value)}
                    />
                    <Input
                      label="Trust"
                      type="number"
                      min={1}
                      max={255}
                      required
                      value={trust}
                      onChange={(e) => setTrust(e.target.value)}
                      description="1 · In contacts. 2 · Trusted."
                    />
                  </Cluster>
                  <Button
                    type="submit"
                    disabled={busy || !target.trim() || !name.trim()}
                  >
                    Save contact
                  </Button>
                </Stack>
              </form>
              {saved.map((person) => (
                <Cluster key={person.uuid} justify="between">
                  <Peer person={person} />
                  <Button
                    variant="ghost"
                    aria-label={"Edit " + person.display_name}
                    onClick={() => edit(person)}
                  >
                    Edit
                  </Button>
                </Cluster>
              ))}
              <Button
                variant="secondary"
                disabled={busy}
                onClick={() => void connect(false)}
              >
                Disconnect Silicon Contacts
              </Button>
            </>
          ) : (
            <Button disabled={busy} onClick={() => void connect(true)}>
              Connect Silicon Contacts
            </Button>
          ))}
      </Stack>
    </Section>
  );
}
export function TingSettings() {
  const status = useQuery({
    queryKey: ["mcport-ting"],
    queryFn: async () =>
      (
        await api.get<{ data: { subscribed: boolean } }>(
          path + "/notifications/subscription",
        )
      ).data,
  });
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  async function setSubscription(enabled: boolean) {
    setBusy(true);
    setError("");
    try {
      if (enabled) await api.post(path + "/notifications/subscription");
      else await api.delete(path + "/notifications/subscription");
      await status.refetch();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Section
      title="Share notifications"
      description="Receive connection and directory shares in your Ting inbox."
    >
      <Stack>
        {(error || status.error) && (
          <ErrorBox error={error || message(status.error)} />
        )}
        {status.data?.subscribed ? (
          <>
            <p>Ting notifications are enabled for this account.</p>
            <Button
              variant="secondary"
              disabled={busy}
              onClick={() => void setSubscription(false)}
            >
              Disable Ting notifications
            </Button>
          </>
        ) : (
          <Button onClick={() => void setSubscription(true)} disabled={busy}>
            Enable Ting notifications
          </Button>
        )}
        <a href="https://ting.teamofsilicons.com/">Open Silicon Ting</a>
        <details>
          <summary>Forward to an optional webhook</summary>
          <p>
            Sign in to the Ting CLI and attach your local destination. Your
            inbox also receives the notification.
          </p>
          <code>ting webhook http://localhost:8080/ting</code>
        </details>
      </Stack>
    </Section>
  );
}
type WorkContext = {
  organizations: {
    org_id: string;
    capabilities: { admin: boolean; team_admins: string[] };
  }[];
  share_targets: { type: string; id: string; org_id: string; name: string }[];
};
export function useGroupTargets(enabled: boolean) {
  const query = useQuery({
    queryKey: ["mcport-organizations"],
    enabled,
    queryFn: async () =>
      (await api.get<{ data: WorkContext }>(path + "/organizations")).data,
  });
  return (
    query.data?.share_targets.filter((t) => {
      const caps = query.data.organizations.find(
        (o) => o.org_id === t.org_id,
      )?.capabilities;
      return (
        caps?.admin ||
        (t.type === "team" &&
          caps?.team_admins.includes(t.id.split("/")[1] ?? ""))
      );
    }) ?? []
  );
}
