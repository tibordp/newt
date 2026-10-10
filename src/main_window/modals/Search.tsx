import { useEffect, useRef, useState } from "react";
import {
  commands,
  type EntryKind,
  type ModeFilter,
  type SearchFilters,
  type SearchParams,
  type TimeBound,
  type TimeRange,
} from "../../lib/bindings";
import { safe, tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  DialogError,
  Field,
  FieldFold,
  CheckboxField,
  useAsyncAction,
} from "./primitives";
import {
  formatSize,
  formatTimeBound,
  parseCount,
  parseMode,
  parseSize,
  parseTimeBound,
  type Parsed,
} from "./searchInput";
import styles from "./Search.module.scss";

type SearchProps = CommonDialogProps & ModalDataOf<"search">;

const NO_FILTERS: SearchFilters = {
  kind: "any",
  min_size: null,
  max_size: null,
  modified: { newer_than: null, older_than: null },
  accessed: { newer_than: null, older_than: null },
  created: { newer_than: null, older_than: null },
  user: null,
  group: null,
  mode: null,
  attributes: 0,
  max_depth: null,
  limit: null,
};

/// Ways to pick permissions: presets, then `find -perm`'s comparisons on
/// bits typed in.
const MODE_CHOICES = {
  any: { label: "Any", filter: null },
  executable: {
    label: "Executable by anyone",
    filter: { how: "any_of", bits: 0o111 },
  },
  setid: { label: "Setuid or setgid", filter: { how: "any_of", bits: 0o6000 } },
  writable: {
    label: "Writable by everyone",
    filter: { how: "all_of", bits: 0o002 },
  },
  exactly: { label: "Exactly…", filter: null },
  all_of: { label: "All of…", filter: null },
  any_of: { label: "Any of…", filter: null },
} satisfies Record<string, { label: string; filter: ModeFilter | null }>;
type ModeChoice = keyof typeof MODE_CHOICES;
const CUSTOM_MODES = ["exactly", "all_of", "any_of"] as const;

function modeChoiceOf(mode: ModeFilter | null): ModeChoice {
  if (!mode) return "any";
  const preset = (Object.keys(MODE_CHOICES) as ModeChoice[]).find((key) => {
    const filter = MODE_CHOICES[key].filter;
    return filter?.how === mode.how && filter.bits === mode.bits;
  });
  return preset ?? mode.how;
}

const WINDOWS_ATTRIBUTES: [number, string][] = [
  [0x1, "Read-only"],
  [0x2, "Hidden"],
  [0x4, "System"],
  [0x20, "Archive"],
];

type TimeField = "modified" | "accessed" | "created";
const TIME_FIELDS: [TimeField, string][] = [
  ["modified", "Modified"],
  ["accessed", "Accessed"],
  ["created", "Created"],
];
type TimeTexts = Record<TimeField, { newer: string; older: string }>;

const NAME_HELP =
  "Patterns separated by ;, any of which matches. A word matches anywhere in the name; *, ? or [ make a glob over the whole name; a / makes a glob over the path below this folder (src/**/*.rs). Empty matches everything.";
const EXCLUDE_HELP =
  "Patterns separated by ; of what is neither a result nor searched into: whole names (never parts of one), globs, or paths below this folder.";
const SIZE_HELP =
  "Decimal prefixes (kB, MB, GB) unless an i makes them binary (KiB, MiB); plain numbers are bytes. Folders never match a size bound.";
const TIME_HELP =
  "A span counted back from when the search starts (90min, 3h, 7d, 2w, 6mo, 1y) or a date (2026-01-31). Entries without this time pass.";
const OWNER_HELP = "By name or numeric id.";
const MODE_HELP =
  "Compares permission bits like find -perm: exactly these, all of these, or any of these.";

const fieldId = (field: string) => `search-${field}`;

const boundText = (bound: TimeBound | null) =>
  bound ? formatTimeBound(bound) : "";

export default function SearchDialog({
  path,
  display_path,
  prefill,
  defaults,
  metadata_traits,
  cancel,
  context,
}: SearchProps) {
  // Refine (prefill) restores the live search; a fresh search seeds its
  // toggles from the sticky last-used defaults, and has no filters.
  const initial: SearchParams = prefill ?? {
    name_pattern: null,
    exclude_pattern: null,
    content_pattern: null,
    content_is_regex: defaults.content_is_regex,
    case_sensitive: defaults.case_sensitive,
    whole_word: defaults.whole_word,
    not_containing: false,
    follow_symlinks: defaults.follow_symlinks,
    skip_hidden: defaults.skip_hidden,
    gitignore: defaults.gitignore,
    one_file_system: defaults.one_file_system,
    filters: NO_FILTERS,
    content_size_cap: 10 * 1024 * 1024,
  };
  const f = initial.filters;

  const [namePattern, setNamePattern] = useState(initial.name_pattern ?? "");
  const [excludePattern, setExcludePattern] = useState(
    initial.exclude_pattern ?? "",
  );
  const [contentPattern, setContentPattern] = useState(
    initial.content_pattern ?? "",
  );
  const [toggles, setToggles] = useState({
    content_is_regex: initial.content_is_regex,
    case_sensitive: initial.case_sensitive,
    whole_word: initial.whole_word,
    not_containing: initial.not_containing,
    follow_symlinks: initial.follow_symlinks,
    skip_hidden: initial.skip_hidden,
    gitignore: initial.gitignore,
    one_file_system: initial.one_file_system,
  });
  const [kind, setKind] = useState<EntryKind>(f.kind);
  const [minSize, setMinSize] = useState(
    f.min_size != null ? formatSize(f.min_size) : "",
  );
  const [maxSize, setMaxSize] = useState(
    f.max_size != null ? formatSize(f.max_size) : "",
  );
  const [times, setTimes] = useState<TimeTexts>(() => {
    const texts = (range: TimeRange) => ({
      newer: boundText(range.newer_than),
      older: boundText(range.older_than),
    });
    return {
      modified: texts(f.modified),
      accessed: texts(f.accessed),
      created: texts(f.created),
    };
  });
  const [user, setUser] = useState(f.user ?? "");
  const [group, setGroup] = useState(f.group ?? "");
  const [modeChoice, setModeChoice] = useState<ModeChoice>(
    modeChoiceOf(f.mode),
  );
  const [modeBits, setModeBits] = useState(
    f.mode ? f.mode.bits.toString(8) : "",
  );
  const [attributes, setAttributes] = useState(f.attributes);
  const [maxDepth, setMaxDepth] = useState(f.max_depth?.toString() ?? "");
  const [limit, setLimit] = useState(f.limit?.toString() ?? "");

  // Anything set behind a fold, remembered or refined, opens it.
  const [contentOpen, setContentOpen] = useState(
    !!initial.content_pattern ||
      initial.content_is_regex ||
      initial.whole_word ||
      initial.not_containing,
  );
  const [moreOpen, setMoreOpen] = useState(
    initial.one_file_system || JSON.stringify(f) !== JSON.stringify(NO_FILTERS),
  );
  const inputRef = useRef<HTMLInputElement>(null);

  function toggle(key: keyof typeof toggles) {
    return (checked: boolean) =>
      setToggles((current) => ({ ...current, [key]: checked }));
  }

  /// The filters as typed, or what's wrong with the first that isn't and
  /// the id of its input.
  function readFilters(): SearchFilters | { error: string; field: string } {
    let problem: { error: string; field: string } | null = null;
    const read = <T,>(parsed: Parsed<T>, field: string): T | null => {
      if ("error" in parsed) {
        problem ??= { error: parsed.error, field };
        return null;
      }
      return parsed.value;
    };
    const range = (field: TimeField): TimeRange => ({
      newer_than: read(parseTimeBound(times[field].newer), `${field}-newer`),
      older_than: read(parseTimeBound(times[field].older), `${field}-older`),
    });
    const custom = (CUSTOM_MODES as readonly string[]).includes(modeChoice);
    const bits = custom ? read(parseMode(modeBits), "mode-bits") : null;
    if (custom && bits === null) {
      problem ??= { error: "Type the permission bits", field: "mode-bits" };
    }
    const filters: SearchFilters = {
      kind,
      min_size: read(parseSize(minSize), "min-size"),
      max_size: read(parseSize(maxSize), "max-size"),
      modified: range("modified"),
      accessed: range("accessed"),
      created: range("created"),
      user: metadata_traits.unix_owner ? user.trim() || null : null,
      group: metadata_traits.unix_owner ? group.trim() || null : null,
      mode: !metadata_traits.unix_owner
        ? null
        : custom
          ? { how: modeChoice as ModeFilter["how"], bits: bits ?? 0 }
          : MODE_CHOICES[modeChoice].filter,
      attributes: metadata_traits.windows_attributes ? attributes : 0,
      max_depth: read(parseCount(maxDepth, "Depth"), "depth"),
      limit: read(parseCount(limit, "The limit"), "limit"),
    };
    return problem ?? filters;
  }

  // The input the last error is about, focused once the inputs are
  // enabled again.
  const invalidField = useRef<string | null>(null);

  const { pending, error, run } = useAsyncAction(async () => {
    const filters = readFilters();
    if ("error" in filters) {
      setMoreOpen(true);
      invalidField.current = filters.field;
      return filters.error;
    }
    safe(
      commands.updateRuntimeState("search", {
        case_sensitive: toggles.case_sensitive,
        content_is_regex: toggles.content_is_regex,
        whole_word: toggles.whole_word,
        follow_symlinks: toggles.follow_symlinks,
        skip_hidden: toggles.skip_hidden,
        gitignore: toggles.gitignore,
        one_file_system: toggles.one_file_system,
      }),
    );
    return tryRun(
      commands.mountSearch(context?.pane_handle ?? 0, path, {
        ...initial,
        ...toggles,
        name_pattern: namePattern.trim() || null,
        exclude_pattern: excludePattern.trim() || null,
        content_pattern: contentPattern || null,
        filters,
      }),
    );
  });

  useEffect(() => {
    if (pending || !error || !invalidField.current) return;
    const input = document.getElementById(fieldId(invalidField.current));
    invalidField.current = null;
    if (input instanceof HTMLInputElement) {
      input.focus();
      input.select();
    }
  }, [pending, error]);

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
  }

  useEffect(() => {
    // Select-all on a prefilled pattern (refine flow) so typing replaces
    // it outright while arrow keys still allow tweaking in place.
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const textProps = {
    type: "text",
    autoComplete: "off",
    autoCorrect: "off",
    autoCapitalize: "off",
    spellCheck: false,
    disabled: pending,
  } as const;
  const unix = metadata_traits.unix_owner;
  const customMode = (CUSTOM_MODES as readonly string[]).includes(modeChoice);

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title={`Search in ${display_path}`} />
      <DialogBody>
        <Field label="Name" htmlFor="name-pattern" title={NAME_HELP}>
          <input
            ref={inputRef}
            id="name-pattern"
            value={namePattern}
            onChange={(e) => setNamePattern(e.target.value)}
            placeholder="*.rs; Cargo.*"
            title={NAME_HELP}
            size={40}
            {...textProps}
          />
        </Field>
        <Field label="Exclude" htmlFor="exclude-pattern" title={EXCLUDE_HELP}>
          <input
            id="exclude-pattern"
            value={excludePattern}
            onChange={(e) => setExcludePattern(e.target.value)}
            placeholder="node_modules; *.min.js"
            title={EXCLUDE_HELP}
            size={40}
            {...textProps}
          />
        </Field>
        <div className={styles.toggles}>
          <CheckboxField
            label="Case-sensitive"
            title="Applies to name and exclude patterns and to content."
            checked={toggles.case_sensitive}
            onChange={toggle("case_sensitive")}
            disabled={pending}
          />
          <CheckboxField
            label="Follow symlinks"
            title="Search inside folders reached through symbolic links. Links that loop back are skipped."
            checked={toggles.follow_symlinks}
            onChange={toggle("follow_symlinks")}
            disabled={pending}
          />
          <CheckboxField
            label="Skip hidden entries"
            title="Hidden files and folders are neither results nor searched into."
            checked={toggles.skip_hidden}
            onChange={toggle("skip_hidden")}
            disabled={pending}
          />
          <CheckboxField
            label="Skip what .gitignore ignores"
            title="Honours .gitignore files and .git/info/exclude, and skips .git itself."
            checked={toggles.gitignore}
            onChange={toggle("gitignore")}
            disabled={pending}
          />
        </div>
        <FieldFold
          summary="Content"
          open={contentOpen}
          onToggle={setContentOpen}
        >
          <div className={styles.foldBody}>
            <input
              id="content-pattern"
              aria-label="Content"
              value={contentPattern}
              onChange={(e) => setContentPattern(e.target.value)}
              placeholder="fn main"
              title="Only files containing this are results. Folders, and files over 10 MiB, are passed over."
              size={40}
              {...textProps}
            />
            <div className={styles.toggles}>
              <CheckboxField
                label="Regular expression"
                checked={toggles.content_is_regex}
                onChange={toggle("content_is_regex")}
                disabled={pending}
              />
              <CheckboxField
                label="Whole words"
                title="The match must start and end at word boundaries."
                checked={toggles.whole_word}
                onChange={toggle("whole_word")}
                disabled={pending}
              />
              <CheckboxField
                label="Files not containing it"
                title="The results are the files the content is not in."
                checked={toggles.not_containing}
                onChange={toggle("not_containing")}
                disabled={pending}
              />
            </div>
          </div>
        </FieldFold>
        <FieldFold
          summary="More search options"
          open={moreOpen}
          onToggle={setMoreOpen}
        >
          <div className={styles.filters}>
            <label htmlFor="search-kind">Type</label>
            <select
              id="search-kind"
              className={styles.wide}
              value={kind}
              onChange={(e) => setKind(e.target.value as EntryKind)}
              disabled={pending}
            >
              <option value="any">Files and folders</option>
              <option value="file">Files</option>
              <option value="directory">Folders</option>
              <option value="symlink">Symbolic links</option>
            </select>

            <span title={SIZE_HELP}>Size</span>
            <label className={styles.captioned} title={SIZE_HELP}>
              <span>at least</span>
              <input
                id={fieldId("min-size")}
                value={minSize}
                onChange={(e) => setMinSize(e.target.value)}
                placeholder="10 MB"
                {...textProps}
              />
            </label>
            <label className={styles.captioned} title={SIZE_HELP}>
              <span>at most</span>
              <input
                id={fieldId("max-size")}
                value={maxSize}
                onChange={(e) => setMaxSize(e.target.value)}
                placeholder="1.5 GiB"
                {...textProps}
              />
            </label>

            {TIME_FIELDS.map(([field, label]) => (
              <TimeRow
                key={field}
                field={field}
                label={label}
                value={times[field]}
                onChange={(value) =>
                  setTimes((current) => ({ ...current, [field]: value }))
                }
                inputProps={textProps}
              />
            ))}

            {metadata_traits.windows_attributes ? (
              <>
                <span title="A result must have every attribute ticked.">
                  Attributes
                </span>
                <div className={styles.attributes}>
                  {WINDOWS_ATTRIBUTES.map(([bit, label]) => (
                    <CheckboxField
                      key={bit}
                      label={label}
                      checked={(attributes & bit) !== 0}
                      onChange={(checked) =>
                        setAttributes((a) => (checked ? a | bit : a & ~bit))
                      }
                      disabled={pending}
                    />
                  ))}
                </div>
              </>
            ) : (
              <>
                <span title={OWNER_HELP}>Owner</span>
                <label className={styles.captioned} title={OWNER_HELP}>
                  <span>user</span>
                  <input
                    value={user}
                    onChange={(e) => setUser(e.target.value)}
                    placeholder="root"
                    {...textProps}
                    disabled={pending || !unix}
                  />
                </label>
                <label className={styles.captioned} title={OWNER_HELP}>
                  <span>group</span>
                  <input
                    value={group}
                    onChange={(e) => setGroup(e.target.value)}
                    placeholder="staff"
                    {...textProps}
                    disabled={pending || !unix}
                  />
                </label>

                <label htmlFor="search-mode" title={MODE_HELP}>
                  Permissions
                </label>
                <select
                  id="search-mode"
                  className={customMode ? undefined : styles.wide}
                  value={modeChoice}
                  onChange={(e) => setModeChoice(e.target.value as ModeChoice)}
                  title={MODE_HELP}
                  disabled={pending || !unix}
                >
                  {(Object.keys(MODE_CHOICES) as ModeChoice[]).map((key) => (
                    <option key={key} value={key}>
                      {MODE_CHOICES[key].label}
                    </option>
                  ))}
                </select>
                {customMode && (
                  <input
                    aria-label="Permission bits (octal)"
                    id={fieldId("mode-bits")}
                    value={modeBits}
                    onChange={(e) => setModeBits(e.target.value)}
                    placeholder="755"
                    title="Octal permission bits"
                    {...textProps}
                    disabled={pending || !unix}
                  />
                )}
              </>
            )}

            <span>Limits</span>
            <label
              className={styles.captioned}
              title="How many levels deep to search; 1 is this folder's own entries."
            >
              <span>depth</span>
              <input
                id={fieldId("depth")}
                value={maxDepth}
                onChange={(e) => setMaxDepth(e.target.value)}
                placeholder="3"
                {...textProps}
              />
            </label>
            <label
              className={styles.captioned}
              title="Stop the search once it has this many results."
            >
              <span>results</span>
              <input
                id={fieldId("limit")}
                value={limit}
                onChange={(e) => setLimit(e.target.value)}
                placeholder="1000"
                {...textProps}
              />
            </label>
          </div>
          <div className={styles.traversal}>
            <CheckboxField
              label="Stay on one filesystem"
              title="Mount points inside the searched folder are not entered."
              checked={toggles.one_file_system}
              onChange={toggle("one_file_system")}
              disabled={pending}
            />
          </div>
        </FieldFold>
        <DialogError error={error} />
      </DialogBody>
      <DialogFooter onCancel={cancel} cancelDisabled={pending}>
        <DialogSubmitButton pending={pending} pendingLabel="Starting…">
          Search
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}

function TimeRow({
  field,
  label,
  value,
  onChange,
  inputProps,
}: {
  field: TimeField;
  label: string;
  value: { newer: string; older: string };
  onChange: (value: { newer: string; older: string }) => void;
  inputProps: React.InputHTMLAttributes<HTMLInputElement>;
}) {
  return (
    <>
      <span title={TIME_HELP}>{label}</span>
      <label className={styles.captioned} title={TIME_HELP}>
        <span>newer than</span>
        <input
          id={fieldId(`${field}-newer`)}
          value={value.newer}
          onChange={(e) => onChange({ ...value, newer: e.target.value })}
          placeholder="7d"
          {...inputProps}
        />
      </label>
      <label className={styles.captioned} title={TIME_HELP}>
        <span>older than</span>
        <input
          id={fieldId(`${field}-older`)}
          value={value.older}
          onChange={(e) => onChange({ ...value, older: e.target.value })}
          placeholder="2026-01-31"
          {...inputProps}
        />
      </label>
    </>
  );
}
