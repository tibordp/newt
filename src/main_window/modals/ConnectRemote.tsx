import { useEffect, useMemo, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  commands,
  type ConnectionKind,
  type ContainerEntry,
  type KubePodEntry,
  type OpenIn,
  type SshHostEntry,
} from "../../lib/bindings";
import { safe, safeSilent, tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import { useAsyncAction } from "./useAsyncAction";
import { DialogError, DialogSubmitButton } from "./DialogActions";
import dialogStyles from "./Dialog.module.scss";
import styles from "./ConnectRemote.module.scss";

type ConnectRemoteProps = CommonDialogProps &
  ModalDataOf<"connect_remote"> & {
    /// Live connect/bootstrap transcript of the mount in flight
    /// (pane-scoped mounts only; window connects log into the new
    /// window's connection screen).
    mountLog?: string[];
  };

type TransportTag = "ssh" | "docker" | "podman" | "kube" | "custom";

// Form fields are kept flat (one shape per transport) rather than as a tagged
// union so that switching transport types preserves user input.
type FormState = {
  transport: TransportTag;
  // SSH
  sshHost: string;
  sshForwardAgent: boolean;
  // Docker / Podman
  containerName: string;
  containerUser: string;
  bootstrapless: boolean;
  // Kubernetes
  kubeContext: string;
  kubeNamespace: string;
  kubePod: string;
  kubeContainer: string;
  // Custom
  customCommand: string;
  customSkipBootstrap: boolean;
  // Session scope
  openIn: OpenIn;
  // Profile
  saveProfile: boolean;
  connectionName: string;
};

function initialForm(
  initial: ConnectionKind,
  defaultOpenIn: OpenIn,
): FormState {
  const base: FormState = {
    transport: "ssh",
    sshHost: "",
    sshForwardAgent: false,
    containerName: "",
    containerUser: "",
    // Docker/Podman containers are typically local — bootstrapless is faster
    // and works for sh-less images. Users opt back into the sh-bootstrap path
    // for cached re-connects.
    bootstrapless: true,
    kubeContext: "",
    kubeNamespace: "",
    kubePod: "",
    kubeContainer: "",
    customCommand: "",
    customSkipBootstrap: false,
    openIn: defaultOpenIn,
    saveProfile: false,
    connectionName: "",
  };
  switch (initial.type) {
    case "ssh":
      return {
        ...base,
        transport: "ssh",
        sshHost: initial.host,
        sshForwardAgent: !!initial.forward_agent,
      };
    case "docker":
      return {
        ...base,
        transport: "docker",
        containerName: initial.container,
        containerUser: initial.user ?? "",
        bootstrapless: !!initial.bootstrapless,
      };
    case "podman":
      return {
        ...base,
        transport: "podman",
        containerName: initial.container,
        containerUser: initial.user ?? "",
        bootstrapless: !!initial.bootstrapless,
      };
    case "kube":
      return {
        ...base,
        transport: "kube",
        kubeContext: initial.context ?? "",
        kubeNamespace: initial.namespace ?? "",
        kubePod: initial.pod,
        kubeContainer: initial.container ?? "",
      };
    case "custom":
      return {
        ...base,
        transport: "custom",
        customCommand: initial.command,
        customSkipBootstrap: !!initial.skip_bootstrap,
      };
    default:
      return base;
  }
}

function buildKind(form: FormState): ConnectionKind | string {
  switch (form.transport) {
    case "ssh":
      if (!form.sshHost.trim()) return "Host is required";
      return {
        type: "ssh",
        host: form.sshHost.trim(),
        forward_agent: form.sshForwardAgent,
      };
    case "docker":
      if (!form.containerName.trim()) return "Container is required";
      return {
        type: "docker",
        container: form.containerName.trim(),
        user: form.containerUser.trim() || null,
        bootstrapless: form.bootstrapless,
      };
    case "podman":
      if (!form.containerName.trim()) return "Container is required";
      return {
        type: "podman",
        container: form.containerName.trim(),
        user: form.containerUser.trim() || null,
        bootstrapless: form.bootstrapless,
      };
    case "kube":
      if (!form.kubePod.trim()) return "Pod is required";
      return {
        type: "kube",
        context: form.kubeContext.trim() || null,
        namespace: form.kubeNamespace.trim() || null,
        pod: form.kubePod.trim(),
        container: form.kubeContainer.trim() || null,
      };
    case "custom": {
      const cmd = form.customCommand.trim();
      if (!cmd) return "Command is required";
      return {
        type: "custom",
        command: cmd,
        skip_bootstrap: form.customSkipBootstrap,
      };
    }
  }
}

function defaultProfileName(form: FormState): string {
  switch (form.transport) {
    case "ssh":
      return form.sshHost;
    case "docker":
      return `docker:${form.containerName}`;
    case "podman":
      return `podman:${form.containerName}`;
    case "kube":
      return `kube:${form.kubeNamespace || ""}${form.kubeNamespace ? "/" : ""}${form.kubePod}`;
    case "custom":
      return form.customCommand.split(/\s+/)[0] ?? "custom";
  }
}

export default function ConnectRemote({
  initial,
  default_open_in,
  cancel,
  context,
  mountLog,
}: ConnectRemoteProps) {
  const [form, setForm] = useState<FormState>(() =>
    initialForm(initial, default_open_in),
  );
  const firstInputRef = useRef<HTMLInputElement>(null);

  function update<K extends keyof FormState>(key: K, value: FormState[K]) {
    setForm((f) => {
      const next = { ...f, [key]: value };
      if (!f.saveProfile) {
        next.connectionName = defaultProfileName(next);
      }
      return next;
    });
  }

  // Re-focus the first input when the user switches transport. Keyboard users
  // expect the dialog to keep them in the active field, not bouncing to body.
  useEffect(() => {
    firstInputRef.current?.focus();
  }, [form.transport]);

  const { pending, error, run } = useAsyncAction(async () => {
    const kind = buildKind(form);
    if (typeof kind === "string") return kind;
    if (form.saveProfile && form.connectionName.trim()) {
      const id = form.connectionName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
      await safeSilent(
        commands.cmdSaveConnection(
          {
            id,
            name: form.connectionName.trim(),
            open_in: form.openIn,
            ...kind,
          },
          null,
        ),
      );
    }
    const err = await tryRun(
      commands.connectTarget(context?.pane_handle ?? 0, kind, form.openIn),
    );
    if (err) return err;
    await safe(commands.closeModal());
    return null;
  });

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
  }

  const TABS: { tag: TransportTag; label: string }[] = [
    { tag: "ssh", label: "SSH" },
    { tag: "docker", label: "Docker" },
    { tag: "podman", label: "Podman" },
    { tag: "kube", label: "Kubernetes" },
    { tag: "custom", label: "Custom" },
  ];

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Connect
        </Dialog.Title>
        <div className={styles.tabBar} role="tablist">
          {TABS.map((t) => (
            <button
              key={t.tag}
              type="button"
              role="tab"
              aria-selected={form.transport === t.tag}
              className={
                form.transport === t.tag ? styles.tabActive : styles.tab
              }
              onClick={() => update("transport", t.tag)}
              disabled={pending}
            >
              {t.label}
            </button>
          ))}
        </div>
        <div
          className={
            form.transport === "custom" ? styles.layoutCompact : styles.layout
          }
        >
          <div className={styles.formColumn}>
            {form.transport === "ssh" && (
              <SshFormFields
                form={form}
                update={update}
                pending={pending}
                firstInputRef={firstInputRef}
              />
            )}
            {(form.transport === "docker" || form.transport === "podman") && (
              <ContainerFormFields
                form={form}
                update={update}
                pending={pending}
                firstInputRef={firstInputRef}
                engine={form.transport}
              />
            )}
            {form.transport === "kube" && (
              <KubeFormFields
                form={form}
                update={update}
                pending={pending}
                firstInputRef={firstInputRef}
              />
            )}
            {form.transport === "custom" && (
              <CustomFormFields
                form={form}
                update={update}
                pending={pending}
                firstInputRef={firstInputRef}
              />
            )}

            <div>
              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: "var(--space-2)",
                  fontSize: "0.9em",
                }}
              >
                <input
                  type="checkbox"
                  checked={form.saveProfile}
                  onChange={(e) =>
                    setForm((f) => ({
                      ...f,
                      saveProfile: e.target.checked,
                      connectionName: f.connectionName || defaultProfileName(f),
                    }))
                  }
                  disabled={pending}
                />
                Save as connection profile
              </label>
              {form.saveProfile && (
                <input
                  type="text"
                  value={form.connectionName}
                  onChange={(e) =>
                    setForm((f) => ({ ...f, connectionName: e.target.value }))
                  }
                  placeholder="Connection name"
                  className={styles.input}
                  style={{ marginTop: "var(--space-2)" }}
                  disabled={pending}
                />
              )}
            </div>
            <MountLogView
              lines={mountLog}
              visible={pending && form.openIn === "pane"}
            />
            <DialogError error={error} />
          </div>

          {form.transport !== "custom" && (
            <div className={styles.listColumn}>
              <DiscoveryPanel
                transport={form.transport}
                form={form}
                setForm={setForm}
                defaultProfileName={defaultProfileName}
              />
            </div>
          )}
        </div>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <label
          style={{
            display: "flex",
            alignItems: "center",
            gap: "var(--space-1)",
            marginRight: "auto",
            fontSize: "0.9em",
          }}
          title="Checked: open a full remote session in a new window. Unchecked: mount the target's filesystem in the active pane — the connection is made by the current session, with its ssh/docker/kubectl, credentials, and network."
        >
          <input
            type="checkbox"
            checked={form.openIn === "window"}
            onChange={(e) =>
              update("openIn", e.target.checked ? "window" : "pane")
            }
            disabled={pending}
          />
          Open as a new session
        </label>
        <button type="button" onClick={cancel} disabled={pending}>
          Cancel
        </button>
        <DialogSubmitButton pending={pending} pendingLabel="Connecting…">
          Connect
        </DialogSubmitButton>
      </div>
    </form>
  );
}

/// Streaming connect/bootstrap log, shown while a pane mount is in
/// flight. Failures don't need it live — the error message carries the
/// transcript.
function MountLogView({
  lines,
  visible,
}: {
  lines?: string[];
  visible: boolean;
}) {
  const boxRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = boxRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);
  if (!visible || !lines || lines.length === 0) return null;
  return (
    <div
      ref={boxRef}
      style={{
        maxHeight: "7em",
        overflowY: "auto",
        fontFamily: "var(--font-mono, monospace)",
        fontSize: "0.8em",
        opacity: 0.75,
        whiteSpace: "pre-wrap",
        overflowWrap: "anywhere",
      }}
    >
      {lines.map((l, i) => (
        <div key={i}>{l}</div>
      ))}
    </div>
  );
}

// --- Form fields (left column) --------------------------------------------

type FieldProps = {
  form: FormState;
  update: <K extends keyof FormState>(key: K, value: FormState[K]) => void;
  pending: boolean;
  firstInputRef: React.RefObject<HTMLInputElement | null>;
};

function SshFormFields({ form, update, pending, firstInputRef }: FieldProps) {
  return (
    <>
      <div>
        <label htmlFor="ssh-host">Host (user@host)</label>
        <input
          ref={firstInputRef}
          id="ssh-host"
          type="text"
          value={form.sshHost}
          onChange={(e) => update("sshHost", e.target.value)}
          className={styles.input}
          disabled={pending}
        />
      </div>
      <label
        style={{
          display: "flex",
          alignItems: "center",
          gap: "var(--space-2)",
          fontSize: "0.9em",
        }}
      >
        <input
          type="checkbox"
          checked={form.sshForwardAgent}
          onChange={(e) => update("sshForwardAgent", e.target.checked)}
          disabled={pending}
        />
        Forward SSH agent (<code>-A</code>)
      </label>
    </>
  );
}

function ContainerFormFields({
  form,
  update,
  pending,
  firstInputRef,
  engine,
}: FieldProps & { engine: "docker" | "podman" }) {
  return (
    <>
      <div>
        <label htmlFor="ctr-name">Container</label>
        <input
          ref={firstInputRef}
          id="ctr-name"
          type="text"
          value={form.containerName}
          onChange={(e) => update("containerName", e.target.value)}
          className={styles.input}
          disabled={pending}
        />
      </div>
      <div>
        <label htmlFor="ctr-user">User (optional)</label>
        <input
          id="ctr-user"
          type="text"
          value={form.containerUser}
          onChange={(e) => update("containerUser", e.target.value)}
          className={styles.input}
          disabled={pending}
        />
      </div>
      <label
        style={{
          display: "flex",
          alignItems: "center",
          gap: "var(--space-2)",
          fontSize: "0.9em",
        }}
      >
        <input
          type="checkbox"
          checked={form.bootstrapless}
          onChange={(e) => update("bootstrapless", e.target.checked)}
          disabled={pending}
        />
        Bootstrapless (use {engine} cp; for containers without sh)
      </label>
    </>
  );
}

function KubeFormFields({ form, update, pending, firstInputRef }: FieldProps) {
  return (
    <>
      <div>
        <label htmlFor="kube-ctx">Context (optional)</label>
        <input
          id="kube-ctx"
          type="text"
          value={form.kubeContext}
          onChange={(e) => update("kubeContext", e.target.value)}
          className={styles.input}
          disabled={pending}
        />
      </div>
      <div>
        <label htmlFor="kube-ns">Namespace (optional)</label>
        <input
          id="kube-ns"
          type="text"
          value={form.kubeNamespace}
          onChange={(e) => update("kubeNamespace", e.target.value)}
          className={styles.input}
          disabled={pending}
        />
      </div>
      <div>
        <label htmlFor="kube-pod">Pod</label>
        <input
          ref={firstInputRef}
          id="kube-pod"
          type="text"
          value={form.kubePod}
          onChange={(e) => update("kubePod", e.target.value)}
          className={styles.input}
          disabled={pending}
        />
      </div>
      <div>
        <label htmlFor="kube-container">Container (optional)</label>
        <input
          id="kube-container"
          type="text"
          value={form.kubeContainer}
          onChange={(e) => update("kubeContainer", e.target.value)}
          className={styles.input}
          disabled={pending}
        />
      </div>
    </>
  );
}

function CustomFormFields({
  form,
  update,
  pending,
  firstInputRef,
}: FieldProps) {
  return (
    <>
      <div>
        <label htmlFor="custom-cmd">Command</label>
        <input
          ref={firstInputRef}
          id="custom-cmd"
          type="text"
          value={form.customCommand}
          onChange={(e) => update("customCommand", e.target.value)}
          className={styles.input}
          placeholder={
            form.customSkipBootstrap
              ? "e.g. my-prespawned-agent"
              : 'e.g. ssh user@host "$NEWT_BOOTSTRAP"'
          }
          disabled={pending}
        />
        <div style={{ fontSize: "0.85em", opacity: 0.6, marginTop: 4 }}>
          {form.customSkipBootstrap ? (
            <>
              Runs locally via <code>sh -c</code>. Stdio is piped straight to
              the RPC layer — your command must produce a ready agent.
            </>
          ) : (
            <>
              Runs locally via <code>sh -c</code>. The bootstrap script is
              exposed as <code>$NEWT_BOOTSTRAP</code>; splice it in however you
              like.
            </>
          )}
        </div>
      </div>
      <label
        style={{
          display: "flex",
          alignItems: "center",
          gap: "var(--space-2)",
          fontSize: "0.9em",
        }}
      >
        <input
          type="checkbox"
          checked={form.customSkipBootstrap}
          onChange={(e) => update("customSkipBootstrap", e.target.checked)}
          disabled={pending}
        />
        Skip bootstrap (assume command already runs the agent)
      </label>
    </>
  );
}

// --- Discovery panel (right column) ---------------------------------------

type DiscoveryPanelProps = {
  transport: TransportTag;
  form: FormState;
  setForm: React.Dispatch<React.SetStateAction<FormState>>;
  defaultProfileName: (f: FormState) => string;
};

function DiscoveryPanel(props: DiscoveryPanelProps) {
  switch (props.transport) {
    case "ssh":
      return <SshList {...props} />;
    case "docker":
      return <ContainerList {...props} engine="docker" />;
    case "podman":
      return <ContainerList {...props} engine="podman" />;
    case "kube":
      return <KubeList {...props} />;
    case "custom":
      return null;
  }
}

function ListHeader({ title, count }: { title: string; count: number | null }) {
  return (
    <div className={styles.listHeader}>
      <span>{title}</span>
      <span>{count === null ? "" : `${count}`}</span>
    </div>
  );
}

function selectFormUpdate(
  setForm: React.Dispatch<React.SetStateAction<FormState>>,
  fn: (f: FormState) => Partial<FormState>,
  defaultProfileName: (f: FormState) => string,
) {
  setForm((f) => {
    const merged = { ...f, ...fn(f) };
    if (!f.saveProfile) merged.connectionName = defaultProfileName(merged);
    return merged;
  });
}

function SshList({ form, setForm, defaultProfileName }: DiscoveryPanelProps) {
  const [hosts, setHosts] = useState<SshHostEntry[]>([]);
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    setLoading(true);
    (async () => {
      const r = await commands.discoverSshHosts(form.openIn);
      if (r.status === "ok") setHosts(r.data.items);
      setLoading(false);
    })();
  }, [form.openIn]);
  const pick = (h: SshHostEntry) => {
    const value = h.user ? `${h.user}@${h.host}` : h.host;
    selectFormUpdate(setForm, () => ({ sshHost: value }), defaultProfileName);
  };
  return (
    <>
      <ListHeader title="~/.ssh/config hosts" count={hosts.length} />
      <div className={styles.listBody}>
        {loading ? (
          <div className={styles.listEmpty}>Loading…</div>
        ) : hosts.length === 0 ? (
          <div className={styles.listEmpty}>No hosts in ~/.ssh/config.</div>
        ) : (
          hosts.map((h) => {
            const value = h.user ? `${h.user}@${h.host}` : h.host;
            const selected = form.sshHost.trim() === value;
            return (
              <button
                type="button"
                key={`${h.host}-${h.user ?? ""}`}
                className={`${styles.row}${selected ? " " + styles.selected : ""}`}
                onClick={() => pick(h)}
              >
                <span className={styles.rowTitle}>{value}</span>
                {h.hostname && h.hostname !== h.host && (
                  <span className={styles.rowSub}>→ {h.hostname}</span>
                )}
              </button>
            );
          })
        )}
      </div>
    </>
  );
}

function ContainerList({
  form,
  setForm,
  defaultProfileName,
  engine,
}: DiscoveryPanelProps & { engine: "docker" | "podman" }) {
  const [items, setItems] = useState<ContainerEntry[]>([]);
  const [warning, setWarning] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    setLoading(true);
    (async () => {
      const r =
        engine === "docker"
          ? await commands.discoverDockerContainers(form.openIn)
          : await commands.discoverPodmanContainers(form.openIn);
      if (r.status === "ok") {
        setItems(r.data.items);
        setWarning(r.data.warning ?? null);
      }
      setLoading(false);
    })();
  }, [engine, form.openIn]);
  const pick = (c: ContainerEntry) => {
    selectFormUpdate(
      setForm,
      () => ({ containerName: c.name || c.id }),
      defaultProfileName,
    );
  };
  // Running containers first, then sorted by name.
  const sorted = useMemo(() => {
    return [...items].sort((a, b) => {
      const ar = a.state.toLowerCase().includes("running") ? 0 : 1;
      const br = b.state.toLowerCase().includes("running") ? 0 : 1;
      if (ar !== br) return ar - br;
      return a.name.localeCompare(b.name);
    });
  }, [items]);
  return (
    <>
      <ListHeader
        title={engine === "docker" ? "Docker containers" : "Podman containers"}
        count={warning ? null : items.length}
      />
      <div className={styles.listBody}>
        {loading ? (
          <div className={styles.listEmpty}>Loading…</div>
        ) : warning ? (
          <div className={styles.listEmpty}>{warning}</div>
        ) : sorted.length === 0 ? (
          <div className={styles.listEmpty}>No containers found.</div>
        ) : (
          sorted.map((c) => {
            const selected = form.containerName.trim() === (c.name || c.id);
            return (
              <button
                type="button"
                key={c.id || c.name}
                className={`${styles.row}${selected ? " " + styles.selected : ""}`}
                onClick={() => pick(c)}
              >
                <span className={styles.rowTitle}>{c.name || c.id}</span>
                <span className={styles.rowSub}>
                  {c.image}
                  {c.state ? ` · ${c.state}` : ""}
                </span>
              </button>
            );
          })
        )}
      </div>
    </>
  );
}

function KubeList({ form, setForm, defaultProfileName }: DiscoveryPanelProps) {
  const [contexts, setContexts] = useState<string[]>([]);
  const [pods, setPods] = useState<KubePodEntry[]>([]);
  const [warning, setWarning] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    (async () => {
      const r = await commands.discoverKubeContexts(form.openIn);
      if (r.status === "ok") setContexts(r.data.items);
    })();
  }, [form.openIn]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    (async () => {
      const r = await commands.discoverKubePods(
        form.openIn,
        form.kubeContext.trim() || null,
        form.kubeNamespace.trim() || null,
      );
      if (cancelled) return;
      if (r.status === "ok") {
        setPods(r.data.items);
        setWarning(r.data.warning ?? null);
      }
      setLoading(false);
    })();
    return () => {
      cancelled = true;
    };
  }, [form.kubeContext, form.kubeNamespace, form.openIn]);

  const pick = (p: KubePodEntry) => {
    selectFormUpdate(
      setForm,
      () => ({
        kubeNamespace: p.namespace,
        kubePod: p.name,
        kubeContainer:
          p.containers.length === 1 ? p.containers[0] : form.kubeContainer,
      }),
      defaultProfileName,
    );
  };

  const pickContext = (ctx: string) => {
    selectFormUpdate(setForm, () => ({ kubeContext: ctx }), defaultProfileName);
  };

  return (
    <>
      <ListHeader
        title="Kubernetes pods"
        count={warning ? null : pods.length}
      />
      {contexts.length > 1 && (
        <div className={styles.contextChips}>
          {contexts.map((c) => (
            <button
              type="button"
              key={c}
              onClick={() => pickContext(c)}
              className={`${styles.chip}${form.kubeContext === c ? " " + styles.chipActive : ""}`}
            >
              {c}
            </button>
          ))}
        </div>
      )}
      <div className={styles.listBody}>
        {loading ? (
          <div className={styles.listEmpty}>Loading…</div>
        ) : warning ? (
          <div className={styles.listEmpty}>{warning}</div>
        ) : pods.length === 0 ? (
          <div className={styles.listEmpty}>No pods found.</div>
        ) : (
          pods.map((p) => {
            const selected =
              form.kubePod.trim() === p.name &&
              (!form.kubeNamespace.trim() ||
                form.kubeNamespace.trim() === p.namespace);
            return (
              <button
                type="button"
                key={`${p.namespace}/${p.name}`}
                className={`${styles.row}${selected ? " " + styles.selected : ""}`}
                onClick={() => pick(p)}
              >
                <span className={styles.rowTitle}>
                  {p.namespace}/{p.name}
                </span>
                {p.containers.length > 0 && (
                  <span className={styles.rowSub}>
                    {p.containers.join(", ")}
                  </span>
                )}
              </button>
            );
          })
        )}
      </div>
    </>
  );
}
