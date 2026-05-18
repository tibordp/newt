import { useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";

import { commands, type WslDistro } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { Palette, Highlight, fuzzyMatch } from "./Palette";
import styles from "./HotPaths.module.scss";

type SelectWslDistroProps = {
  distros: WslDistro[];
};

const preventAutoFocus = (e: Event) => e.preventDefault();

export default function SelectWslDistro({ distros }: SelectWslDistroProps) {
  const [filter, setFilter] = useState("");

  const filtered = useMemo(() => {
    return distros
      .map((d) => ({ distro: d, ...fuzzyMatch(filter, d.name) }))
      .filter(({ matches }) => matches)
      .sort(
        (a, b) =>
          b.score - a.score ||
          Number(b.distro.is_default) - Number(a.distro.is_default),
      );
  }, [distros, filter]);

  const onSelect = (value: string) => {
    safe(commands.connectWslDistro(value));
  };

  return (
    <Dialog.Content
      className={styles.content}
      onCloseAutoFocus={preventAutoFocus}
    >
      <Dialog.Title className="sr-only">
        Connect to WSL Distribution
      </Dialog.Title>
      <Palette shouldFilter={false}>
        <div className={styles.header}>
          <Command.Input
            value={filter}
            onValueChange={setFilter}
            placeholder="Search WSL distributions..."
          />
        </div>
        <Command.List>
          <Command.Empty>No matching distributions.</Command.Empty>
          {filtered.map(({ distro: d }) => (
            <Command.Item key={d.name} value={d.name} onSelect={onSelect}>
              <div className={styles.itemContent}>
                <span className={styles.name}>
                  <Highlight
                    text={d.name}
                    filter={filter}
                    highlightClass={styles.highlight}
                  />
                </span>
                {d.is_default && <span className={styles.path}>default</span>}
              </div>
            </Command.Item>
          ))}
        </Command.List>
      </Palette>
    </Dialog.Content>
  );
}
