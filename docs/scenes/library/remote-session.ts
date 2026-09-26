import {
  at,
  dir,
  file,
  fsStats,
  gb,
  kb,
  mainWindow,
  pane,
  type VfsInfo,
} from "../builders";
import { docsColumns } from "../demo";
import type { Scene } from "../types";

const E = "\x1b[";
const prompt = `${E}1;32mdemo@build-01${E}0m:${E}1;34m/srv/app/releases${E}0m$ `;
// In a remote session the backend relabels the target's own filesystem
// "Remote" (and the client's, if exposed, "Local").
const REMOTE: VfsInfo = {
  id: 0,
  name: "Remote",
  hostLocal: false,
  display: (p) => p,
  rootLabel: "/",
};

const linux = { user: { name: "deploy" }, group: { name: "deploy" } };

const scene: Scene = {
  description: "A full remote session over SSH: panes and terminal on the host",
  size: { width: 1100, height: 640 },
  ...docsColumns(1100),
  window: "main",
  state: mainWindow({
    title: "Newt [build-01]",
    terminals: [1],
    panes: [
      pane({
        path: "/srv/app/releases",
        vfs: REMOTE,
        focused: "2026-07-28.2",
        fsStats: fsStats(gb(492.1), gb(187.4)),
        entries: [
          dir("2026-07-21.1", {
            ...linux,
            modified: at("2026-07-21T10:14:02"),
          }),
          dir("2026-07-24.1", {
            ...linux,
            modified: at("2026-07-24T16:40:51"),
          }),
          dir("2026-07-28.1", {
            ...linux,
            modified: at("2026-07-28T09:02:13"),
          }),
          dir("2026-07-28.2", {
            ...linux,
            modified: at("2026-07-28T15:31:47"),
          }),
          file("current", {
            ...linux,
            size: 12,
            is_symlink: true,
            symlink_target: "2026-07-28.2",
            modified: at("2026-07-28T15:31:49"),
          }),
          file("deploy.log", {
            ...linux,
            size: kb(48.3),
            modified: at("2026-07-28T15:31:52"),
          }),
        ],
      }),
      pane({
        path: "/var/log/app",
        vfs: REMOTE,
        entries: [
          file("access.log", {
            ...linux,
            size: 18_402_117,
            modified: at("2026-07-28T17:34:58"),
          }),
          file("access.log.1.gz", {
            ...linux,
            size: 2_841_022,
            modified: at("2026-07-27T23:59:59"),
          }),
          file("error.log", {
            ...linux,
            size: 41_206,
            modified: at("2026-07-28T17:12:40"),
          }),
        ],
      }),
    ],
  }),
  terminals: {
    1: [
      `${prompt}systemctl status app --no-pager | head -3`,
      `${E}1;32m●${E}0m app.service - Walker API`,
      `     Loaded: loaded (/etc/systemd/system/app.service; enabled)`,
      `     Active: ${E}1;32mactive (running)${E}0m since Tue 2026-07-28 15:31:55 CEST; 2h 3min ago`,
      prompt,
    ].join("\n"),
  },
};

export default scene;
