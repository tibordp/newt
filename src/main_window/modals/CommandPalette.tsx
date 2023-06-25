import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Command, commands, executeCommand } from "../../lib/commands";
import { MainWindowState } from "../MainWindow";

type CommandPaletteProps = {
  state: MainWindowState;
  onClose: () => void;
};

function Highlight(props: {
  name: string;
  filter: string;
  paneHandle?: number;
}) {
  const { name, filter } = props;
  let key = 0;
  let a = 0;
  let b = 0;
  const parts = [];
  while (a < filter.length && b < name.length) {
    if (filter[a].toLowerCase() === name[b].toLowerCase()) {
      parts.push(
        <span key={key++} className="highlight">
          {name[b]}
        </span>
      );
      a++;
      b++;
    } else {
      parts.push(<span key={key++}>{name[b]}</span>);
      b++;
    }
  }

  if (b < name.length) {
    parts.push(<span key={key++}>{name.slice(b)}</span>);
  }

  return <span>{parts}</span>;
}

export default function CommandPalette({
  state,
  onClose,
}: CommandPaletteProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  const paneHandle =
    state.display_options.panes_focused && state.display_options.active_pane;

  const [filter, setFilter] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);

  useEffect(() => {
    setFilter("");
    setActiveIndex(0);
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    setActiveIndex(0);
  }, [filter]);

  const filteredCommands = useMemo(() => {
    let ret = commands.map((command) => {
      let a = 0;
      let b = 0;
      let consecutive = 0;
      let maxConsecutive = 0;

      while (a < filter.length && b < command.name.length) {
        if (filter[a].toLowerCase() === command.name[b].toLowerCase()) {
          consecutive++;
          a++;
          b++;
        } else {
          maxConsecutive = Math.max(maxConsecutive, consecutive);
          consecutive = 0;
          b++;
        }
      }

      return {
        matches: a === filter.length,
        score: maxConsecutive,
        command: command,
      };
    });

    ret = ret.filter(
      ({ matches, command }) =>
        matches && (command.noPane || !!paneHandle || paneHandle === 0)
    );
    ret.sort((a, b) => a.score - b.score);
    return ret.map(({ command }) => command);
  }, [filter]);

  const onClick = (command: Command) => {
    executeCommand(command, state);
    onClose();
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      setActiveIndex((i) => (i + 1) % filteredCommands.length);
    } else if (e.key === "ArrowUp") {
      setActiveIndex(
        (i) => (i - 1 + filteredCommands.length) % filteredCommands.length
      );
    } else if (e.key === "Enter") {
      onClick(filteredCommands[activeIndex]);
    } else if (e.key === "Escape") {
      onClose();
    } else {
      return;
    }

    e.preventDefault();
  };

  useLayoutEffect(() => {
    if (listRef.current) {
      const activeElement = listRef.current.querySelector(".active");
      if (activeElement) {
        activeElement.scrollIntoView({ block: "nearest" });
      }
    }
  }, [activeIndex]);

  return (
    <>
      <div className="command-palette-header">
        <input
          ref={inputRef}
          onKeyDown={onKeyDown}
          type="text"
          name="path"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          onBlur={onClose}
          placeholder="Start typing to filter commands"
          size={60}
          autoFocus
        />
      </div>
      <ul className="commands" ref={listRef}>
        {!filteredCommands.length && (
          <li>
            <span>No commands found</span>
          </li>
        )}
        {filteredCommands.map((command, i) => (
          <li
            className={`command ${i === activeIndex ? "active" : ""}`}
            onClick={() => onClick(command)}
            key={i}
          >
            <Highlight name={command.name} filter={filter} />
            {command.shortcut && (
              <div className="shortcut">
                {command.shortcut.render().map((e, i) => (
                  <Fragment key={i}>
                    {i !== 0 ? " + " : ""}
                    <kbd>{e}</kbd>
                  </Fragment>
                ))}
              </div>
            )}
          </li>
        ))}
      </ul>
    </>
  );
}
