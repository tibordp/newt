import { useState, useEffect, useRef } from "react";
import iconMapping from "../assets/mapping.json";
import { File, ColumnDef, PaneState, Sorting } from "./types";
import { modeString } from "./utils";

function FileName({ focused, filter, info }) {
  const { name, is_dir, is_symlink, is_hidden } = info;

  const icon =
    iconMapping.light.fileNames[name] ||
    iconMapping.light.fileExtensions[name.substr(name.indexOf(".") + 1)] ||
    iconMapping.light.file;

  const { fontCharacter, fontColor } = iconMapping.iconDefinitions[icon];
  const ch = String.fromCodePoint(parseInt(fontCharacter, 16));

  const nameElement = (
    <>
      {(!focused || filter === null) && <>{name}</>}
      {focused && filter !== null && (
        <>
          <span className="filter-head">{name.substr(0, filter.length)}</span>
          <span className="filter-tail">{name.substr(filter.length)}</span>
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
      className={`filename ${is_hidden ? "hidden-file" : ""} ${is_symlink ? "symlink" : ""
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
    render: (info, { filter, focused, active }) => (
      <FileName
        filter={filter}
        focused={active && focused == info.name}
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
        {info.size !== null
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
  const [startOffset, setStartOffset] = useState(null);

  const onmousedown = (e) => {
    e.preventDefault();
    setStartOffset(ref.current.offsetWidth - e.clientX);
  };

  const onmouseup = (e) => {
    if (startOffset !== null) {
      e.preventDefault();
      setStartOffset(null);
    }
  };

  const onmousemove = (e) => {
    if (startOffset !== null && startOffset + e.clientX > 10) {
      e.preventDefault();
      const root = document.querySelector(":root");
      // @ts-ignore
      root.style.setProperty(
        `--${widthPrefix}-${column.key}`,
        `${startOffset + e.clientX}px`
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
    const root = document.querySelector(":root");
    // @ts-ignore
    root.style.setProperty(
      `--${widthPrefix}-${column.key}`,
      `${column.initialWidth}px`
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
        className={`column`}
        style={{
          width: `var(--${widthPrefix}-${column.key})`,
          textAlign: column.align,
        }}
      >
        {column.subcolumns.map((subcol, i) => (
          <div
            key={i}
            ref={ref}
            className={`subcolumn ${subcol.sortKey ? "sortable" : ""}`}
            onClick={(e: React.MouseEvent) => {
              e.stopPropagation();
              if (subcol.sortKey) {
                onSort(
                  subcol.sortKey,
                  sorting.key != subcol.sortKey || !sorting.asc
                );
              }
            }}
            style={subcol.style || defaultSubcolStyle}
          >
            {column.align == "right" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className="sorting-indicator">▲ </span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className="sorting-indicator">▼ </span>
                )}
              </>
            )}
            {subcol.name}
            {column.align == "left" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className="sorting-indicator"> ▲</span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className="sorting-indicator"> ▼</span>
                )}
              </>
            )}
          </div>
        ))}
      </div>
      <div className="column-grip" onMouseDown={onmousedown}></div>
    </>
  );
}
