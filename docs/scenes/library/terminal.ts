import { at, dir, file, fsStats, gb, mainWindow, pane } from "../builders";
import { docsColumns } from "../demo";
import type { Scene } from "../types";

const E = "\x1b[";
const prompt = `${E}1;32mdemo@studio${E}0m ${E}1;34m~/src/newt${E}0m ${E}33m(master)${E}0m $ `;

const scene: Scene = {
  description: "Both panes above the integrated terminal, focus in the shell",
  size: { width: 1100, height: 768 },
  ...docsColumns(1100),
  window: "main",
  state: mainWindow({
    terminals: [1],
    terminalFocused: true,
    panes: [
      pane({
        path: "/Users/demo/src/newt",
        focused: "src",
        gitBranch: "master",
        fsStats: fsStats(gb(994.66), gb(297.71)),
        entries: [
          dir("docs", { modified: at("2026-07-17T00:58:44") }),
          dir("libs", { modified: at("2026-07-18T23:10:24") }),
          dir("scripts", { modified: at("2026-07-14T18:16:35") }),
          dir("src", { modified: at("2026-07-26T20:21:15") }),
          dir("src-tauri", { modified: at("2026-07-28T17:24:38") }),
          dir("xtask", { modified: at("2026-07-27T11:02:51") }),
          file("Cargo.toml", {
            size: 536,
            modified: at("2026-07-18T22:47:55"),
          }),
          file("package.json", {
            size: 1_941,
            modified: at("2026-07-16T13:10:58"),
          }),
          file("README.md", {
            size: 4_696,
            modified: at("2026-07-27T12:41:41"),
          }),
        ],
      }),
      pane({
        path: "/Users/demo/src/newt/libs",
        focused: "newt-common",
        entries: [
          dir("newt-agent", { modified: at("2026-07-28T16:02:19") }),
          dir("newt-common", { modified: at("2026-07-28T17:11:47") }),
        ],
      }),
    ],
  }),
  terminals: {
    1: [
      `${prompt}git log --oneline -5`,
      `${E}33mf767865${E}0m ${E}1;36m(HEAD -> ${E}1;32mmaster${E}1;36m)${E}0m feat(operations): exclusive writes`,
      `${E}33m716c56f${E}0m feat(operations): conditional overwrites and an up-front conflict answer`,
      `${E}33mea12d82${E}0m feat(vfs): flat S3 listings, prefetch, and reasoned partial results`,
      `${E}33m38b31b8${E}0m refactor(vfs): one depth-first walker behind a Visitor`,
      `${E}33m0e20498${E}0m feat(operations): stop at mount points in delete and recursive attributes`,
      `${prompt}cargo test -p newt-common walk`,
      `${E}1;32m    Finished${E}0m \`test\` profile [unoptimized + debuginfo] target(s) in 0.41s`,
      `${E}1;32m     Running${E}0m unittests src/lib.rs`,
      "",
      "running 12 tests",
      `test result: ${E}32mok${E}0m. 12 passed; 0 failed; 0 ignored; 0 measured; 431 filtered out`,
      "",
      prompt,
    ].join("\n"),
  },
};

export default scene;
