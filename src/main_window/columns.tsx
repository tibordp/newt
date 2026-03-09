import { useState, useEffect, useRef } from "react";
import iconMapping from "../assets/mapping.json";
import { File, ColumnDef, Sorting } from "./types";
import { modeString } from "./utils";
import styles from "./Columns.module.scss";

const fileNames = iconMapping.light.fileNames as Record<string, string>;
const fileExtensions = iconMapping.light.fileExtensions as Record<
  string,
  string
>;
const iconDefinitions = iconMapping.iconDefinitions as unknown as Record<
  string,
  { fontCharacter: string; fontColor: string }
>;

function FileName({
  focused,
  filter,
  filterMode,
  info,
}: {
  focused: boolean;
  filter?: string;
  filterMode: string;
  info: File;
}) {
  const { name, is_dir, is_symlink, is_hidden } = info;

  const icon =
    fileNames[name] ||
    fileExtensions[name.substr(name.indexOf(".") + 1)] ||
    iconMapping.light.file;

  const { fontCharacter, fontColor } = iconDefinitions[icon];
  const ch = String.fromCodePoint(parseInt(fontCharacter, 16));

  const nameElement = (
    <>
      {(!focused || filter == null || filterMode === "filter") && <>{name}</>}
      {focused && filter != null && filterMode !== "filter" && (
        <>
          <span className={styles.filterHead}>
            {name.substr(0, filter.length)}
          </span>
          <span>{name.substr(filter.length)}</span>
        </>
      )}
    </>
  );

  const iconElement = is_dir ? (
    <div className="file-icon folder" />
  ) : (
    <div className="file-icon" style={{ color: fontColor }}>
      {ch}
    </div>
  );

  return (
    <div
      className={`${styles.filename} ${is_hidden ? "hidden-file" : ""} ${
        is_symlink ? "symlink" : ""
      }`}
    >
      {iconElement}
      <div className={focused ? "filename-part focused" : "filename-part"}>
        {nameElement}
      </div>
    </div>
  );
}

export const columns: ColumnDef[] = [
  {
    align: "left",
    key: "name",
    subcolumns: [
      {
        sortKey: "name",
        name: "Name",
        style: {
          flexBasis: "60px",
        },
      },
      {
        sortKey: "extension",
        name: "Ext",
      },
    ],
    render: (info, { isFocused, filter, filterMode }) => (
      <FileName
        filter={filter}
        filterMode={filterMode}
        focused={isFocused}
        info={info}
      />
    ),
    initialWidth: 250,
  },
  {
    align: "right",
    key: "size",
    initialWidth: 100,
    subcolumns: [
      {
        name: "Size",
        sortKey: "size",
      },
    ],
    render: (info) => (
      <>
        {info.size != null
          ? info.size.toLocaleString()
          : info.is_dir
            ? "DIR"
            : "???"}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 80,
    key: "modified_date",
    subcolumns: [
      {
        name: "Date",
        sortKey: "modified",
      },
    ],
    render: (info) => <>{new Date(info.modified).toLocaleDateString()}</>,
  },
  {
    align: "right",
    initialWidth: 80,
    key: "modified_time",
    subcolumns: [
      {
        name: "Time",
        sortKey: "modified",
      },
    ],
    render: (info) => <>{new Date(info.modified).toLocaleTimeString()}</>,
  },
  {
    align: "left",
    initialWidth: 70,
    key: "user",
    subcolumns: [
      {
        name: "User",
        sortKey: "user",
      },
    ],
    render: (info) => <>{info.user?.name || info.user?.id}</>,
  },
  {
    align: "left",
    initialWidth: 70,
    key: "group",
    subcolumns: [
      {
        name: "Group",
        sortKey: "group",
      },
    ],
    render: (info) => <>{info.group?.name || info.group?.id}</>,
  },
  {
    align: "left",
    initialWidth: 70,
    key: "mode",
    subcolumns: [
      {
        name: "Mode",
        sortKey: "mode",
      },
    ],
    render: (info) => <>{modeString(info.mode)}</>,
  },
];

type ColumnHeaderProps = {
  widthPrefix: string;
  column: ColumnDef;
  sorting: Sorting;
  onSort: (key: string, asc: boolean) => void;
};

export function ColumnHeader({
  widthPrefix,
  column,
  sorting,
  onSort,
}: ColumnHeaderProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [startOffset, setStartOffset] = useState<number | null>(null);

  const onmousedown = (e: React.MouseEvent) => {
    e.preventDefault();
    setStartOffset(ref.current!.offsetWidth - e.clientX);
  };

  const onmouseup = (e: MouseEvent) => {
    if (startOffset !== null) {
      e.preventDefault();
      setStartOffset(null);
    }
  };

  const onmousemove = (e: MouseEvent) => {
    if (startOffset !== null && startOffset + e.clientX > 10) {
      e.preventDefault();
      const root = document.querySelector(":root") as HTMLElement;
      root.style.setProperty(
        `--${widthPrefix}-${column.key}`,
        `${startOffset + e.clientX}px`,
      );
    }
  };

  useEffect(() => {
    document.addEventListener("mouseup", onmouseup);
    document.addEventListener("mousemove", onmousemove);

    return () => {
      document.removeEventListener("mouseup", onmouseup);
      document.removeEventListener("mousemove", onmousemove);
    };
  }, [startOffset]);

  useEffect(() => {
    const root = document.querySelector(":root") as HTMLElement;
    root.style.setProperty(
      `--${widthPrefix}-${column.key}`,
      `${column.initialWidth}px`,
    );
  }, []);

  const defaultSubcolStyle = {
    flexGrow: 1,
    flexShrink: 1,
  };

  return (
    <>
      <div
        ref={ref}
        className={styles.column}
        style={{
          width: `var(--${widthPrefix}-${column.key})`,
          textAlign: column.align,
        }}
      >
        {column.subcolumns?.map((subcol, i) => (
          <div
            key={i}
            ref={ref}
            className={`${styles.subcolumn} ${subcol.sortKey ? styles.sortable : ""}`}
            onClick={(e: React.MouseEvent) => {
              e.stopPropagation();
              if (subcol.sortKey) {
                onSort(
                  subcol.sortKey,
                  sorting.key != subcol.sortKey || !sorting.asc,
                );
              }
            }}
            style={subcol.style || defaultSubcolStyle}
          >
            {column.align == "right" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className={styles.sortingIndicator}>▲ </span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className={styles.sortingIndicator}>▼ </span>
                )}
              </>
            )}
            {subcol.name}
            {column.align == "left" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className={styles.sortingIndicator}> ▲</span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className={styles.sortingIndicator}> ▼</span>
                )}
              </>
            )}
          </div>
        ))}
      </div>
      <div className={styles.columnGrip} onMouseDown={onmousedown}></div>
    </>
  );
}
